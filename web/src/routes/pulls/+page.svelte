<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { page } from '$app/state';
  import { afterNavigate, replaceState } from '$app/navigation';
  import { Search, RefreshCw } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import { live } from '#lib/events';
  import { relative } from '#lib/time';
  import { createNotices } from '#lib/notices.svelte';
  import { needsReview, pullPath, reviewing } from '../../lib/pulls';
  import type { PullInbox, PullSummary } from '#lib/types';
  import Notice from '#lib/components/Notice.svelte';
  import SelectField from '#lib/components/SelectField.svelte';
  import PullStatus from '#lib/components/PullStatus.svelte';
  import PullActions from '#lib/components/PullActions.svelte';

  const notice = createNotices();
  let inbox = $state<PullInbox>({ refreshed_at: null, pulls: [] });
  let installed = $state(0);
  let loading = $state(true);
  let refreshing = $state(false);
  let ready = $state(false);
  let search = $state(''),
    q = $state(''),
    repo = $state(''),
    segment = $state('');
  let drafts = $state(true),
    bots = $state(true);
  let generation = 0;
  const groups = [
    { value: '', label: 'All' },
    { value: 'needs_review', label: 'Needs review' },
    { value: 'findings', label: 'Findings' },
    { value: 'reviewing', label: 'Reviewing' },
    { value: 'clean', label: 'Clean' },
  ];
  const repoOptions = $derived([
    { value: '', label: 'All repositories' },
    ...[...new Set(inbox.pulls.map((pull) => pull.repo))]
      .sort()
      .map((name) => ({ value: name, label: name })),
  ]);
  const matches = (pull: PullSummary, value: string) =>
    !value ||
    (value === 'needs_review' ? needsReview.has(pull.review.state) : pull.review.state === value);
  const searched = $derived(
    inbox.pulls.filter((pull) => {
      const term = q.trim().toLowerCase();
      return (
        (!repo || pull.repo === repo) &&
        (drafts || !pull.draft) &&
        (bots || !pull.author_is_bot) &&
        (!term ||
          [pull.title, pull.repo, pull.author].some((value) =>
            value.toLowerCase().includes(term),
          ) ||
          (/^#?\d+$/.test(term) && pull.number === Number(term.replace(/^#/, ''))))
      );
    }),
  );
  const filtered = $derived(searched.filter((pull) => matches(pull, segment)));
  const segments = $derived(
    groups.map((group) => ({
      ...group,
      count: searched.filter((pull) => matches(pull, group.value)).length,
    })),
  );

  function readQuery() {
    const params = page.url.searchParams;
    search = q = params.get('q') || '';
    repo = params.get('repo') || '';
    segment = groups.some((group) => group.value === params.get('state'))
      ? params.get('state')!
      : '';
    drafts = params.get('drafts') !== '0';
    bots = params.get('bots') !== '0';
    ready = true;
  }
  function clearFilters() {
    search = q = repo = segment = '';
    drafts = bots = true;
  }
  afterNavigate(readQuery);
  $effect(() => {
    const value = search;
    const timer = setTimeout(() => (q = value), 250);
    return () => clearTimeout(timer);
  });
  $effect(() => {
    const values = { q, repo, state: segment, drafts: drafts ? '' : '0', bots: bots ? '' : '0' };
    if (!ready) return;
    const url = new URL(window.location.href);
    for (const [key, value] of Object.entries(values)) {
      if (value) url.searchParams.set(key, value);
      else url.searchParams.delete(key);
    }
    if (url.href !== window.location.href) replaceState(url, {});
  });
  async function load() {
    const current = ++generation;
    try {
      const [data, repos] = await Promise.all([api.pulls(), api.repos()]);
      if (current !== generation) return;
      inbox = data;
      installed = repos.length;
      notice.state.error = '';
    } catch (cause) {
      if (current === generation) notice.failure(errorMessage(cause));
    } finally {
      if (current === generation) loading = false;
    }
  }
  async function refresh() {
    refreshing = true;
    try {
      await api.reconcileInstallations();
      await load();
    } catch (cause) {
      notice.failure(errorMessage(cause));
    } finally {
      refreshing = false;
    }
  }
  onMount(() => {
    readQuery();
    void load();
  });
  $effect(() => {
    const { job, fix, repos } = $live;
    if (!job && !fix && !repos) return;
    const timer = setTimeout(() => untrack(() => void load()), 500);
    return () => clearTimeout(timer);
  });
  function queued(pull: PullSummary, id: string) {
    inbox = {
      ...inbox,
      pulls: inbox.pulls.map((item) =>
        item.repo_id === pull.repo_id && item.number === pull.number
          ? { ...item, review: reviewing(item.review, id) }
          : item,
      ),
    };
  }
</script>

<svelte:head><title>Pull requests · Revera</title></svelte:head>
<div class="page-header">
  <div>
    <h1>Pull requests</h1>
    <p>Open pull requests across your repositories.</p>
  </div>
  <div class="refresh-actions">
    {#if inbox.refreshed_at}<time
        class="helper"
        datetime={inbox.refreshed_at}
        title={new Date(inbox.refreshed_at).toLocaleString()}
        >Updated {relative(inbox.refreshed_at).toLowerCase()}</time
      >{/if}
    <button class="button" disabled={refreshing} onclick={() => void refresh()}
      ><RefreshCw size={14} />{refreshing ? 'Refreshing…' : 'Refresh'}</button
    >
  </div>
</div>
<div class="filters pull-filters">
  <div class="search-field">
    <label class="sr-only" for="pull-search">Search pull requests</label><Search size={15} /><input
      id="pull-search"
      type="search"
      bind:value={search}
      placeholder="Search pull requests…"
    />
  </div>
  <SelectField id="pull-repo" label="Repository" bind:value={repo} options={repoOptions} />
  <div class="visibility-filters">
    <label><input type="checkbox" bind:checked={drafts} />Drafts</label><label
      ><input type="checkbox" bind:checked={bots} />Bot authors</label
    >
  </div>
</div>
<div class="inbox-tabs">
  <div class="segmented" role="group" aria-label="Filter by review state">
    {#each segments as item}<button
        aria-pressed={segment === item.value}
        onclick={() => (segment = item.value)}>{item.label} <span>{item.count}</span></button
      >{/each}
  </div>
  <span class="result-count"
    >{filtered.length} {filtered.length === 1 ? 'pull request' : 'pull requests'}</span
  >
</div>
{#if notice.state.error}<Notice message={notice.state.error} retry={load} />{/if}
{#if notice.state.message}<Notice message={notice.state.message} kind="success" />{/if}
{#if loading}<div class="loading" role="status">Loading pull requests…</div>
{:else if filtered.length}
  <ul class="pull-list" aria-label="Open pull requests">
    {#each filtered as pull (`${pull.repo_id}:${pull.number}`)}
      <li class="pull-row" data-pull={`${pull.repo}#${pull.number}`}>
        <div class="pull-state"><PullStatus review={pull.review} /></div>
        <div class="pull-copy">
          <a class="row-title" href={pullPath(pull)}>{pull.title}</a>
          <div class="pull-badges">
            <span class="helper">#{pull.number}</span>{#if pull.draft}<span class="badge"
                >Draft</span
              >{/if}{#if pull.fork}<span class="badge">Fork</span
              >{/if}{#each pull.labels.slice(0, 3) as label}<span class="badge">{label}</span
              >{/each}
          </div>
          <div class="row-meta">
            <span>{pull.repo}</span><span>·</span><span>{pull.author}</span><span>·</span><time
              datetime={pull.updated_at}
              title={new Date(pull.updated_at).toLocaleString()}>{relative(pull.updated_at)}</time
            >
          </div>
        </div>
        <div class="row-actions">
          <PullActions
            {pull}
            onqueued={(id) => queued(pull, id)}
            onrefresh={load}
            onsuccess={notice.success}
            onerror={notice.failure}
          />
        </div>
      </li>
    {/each}
  </ul>
{:else if !notice.state.error}
  <div class="empty-state">
    {#if !installed}No repositories are installed yet.<br /><a class="button" href="/repositories"
        >Connect repositories</a
      >
    {:else if !inbox.pulls.length}No open pull requests.
      <p class="helper">New pull requests will appear here automatically.</p>
    {:else}No pull requests match your filters.<br /><button class="button" onclick={clearFilters}
        >Clear filters</button
      >{/if}
  </div>
{/if}

<style>
  .refresh-actions {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  .pull-filters {
    margin-bottom: 12px;
  }
  .pull-filters :global(.select-trigger) {
    max-width: 260px;
  }
  .visibility-filters {
    display: flex;
    gap: 16px;
    margin-left: auto;
    font-size: 12px;
    color: var(--muted);
  }
  .visibility-filters label {
    display: flex;
    align-items: center;
    gap: 6px;
    white-space: nowrap;
  }
  .inbox-tabs {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 20px;
  }
  .pull-list {
    list-style: none;
    padding: 0;
    margin: 0;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .pull-row {
    display: grid;
    grid-template-columns: 126px minmax(0, 1fr) auto;
    align-items: center;
    gap: 16px;
    padding: 16px;
    border-bottom: 1px solid var(--border);
  }
  .pull-row:last-child {
    border-bottom: 0;
  }
  .pull-row:hover {
    background: var(--surface);
  }
  .pull-copy {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .pull-badges {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-top: 5px;
  }
  .pull-badges .badge {
    max-width: 160px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    display: inline-block;
  }
  @media (max-width: 760px) {
    .refresh-actions {
      gap: 8px;
    }
    .refresh-actions time {
      display: none;
    }
    .pull-filters .search-field {
      width: 100%;
    }
    .pull-filters input[type='search'] {
      width: 100%;
    }
    .visibility-filters {
      margin-left: 0;
    }
    .inbox-tabs .segmented {
      flex-wrap: wrap;
    }
    .pull-row {
      grid-template-columns: minmax(0, 1fr) auto;
      gap: 10px;
    }
    .pull-copy {
      grid-column: 1 / -1;
      grid-row: 2;
    }
    .row-actions {
      grid-column: 2;
      grid-row: 1;
    }
    .pull-state {
      grid-column: 1;
      grid-row: 1;
    }
  }
</style>
