<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { afterNavigate, replaceState } from '$app/navigation';
  import { Search } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import { live } from '#lib/events';
  import type { JobSummary, Repository } from '#lib/types';
  import JobsTable from '#lib/components/JobsTable.svelte';
  import Notice from '#lib/components/Notice.svelte';
  import SelectField from '#lib/components/SelectField.svelte';
  import ExternalReviewDialog from '#lib/components/ExternalReviewDialog.svelte';
  import ExternalTracking from '#lib/components/ExternalTracking.svelte';
  let queued = $state('');
  function onqueued(id: string) {
    queued = id;
    clearFilters();
    void load();
  }
  let jobs = $state<JobSummary[]>([]);
  let repos = $state<Repository[]>([]);
  let repo = $state('');
  let status = $state('');
  let kind = $state('');
  let search = $state('');
  let q = $state('');
  let next = $state<string>();
  let error = $state('');
  let loading = $state(true);
  let ready = $state(false);
  let generation = 0;
  const repoOptions = $derived([
    { value: '', label: 'All repositories' },
    ...repos.map((item) => ({ value: item.full_name, label: item.full_name })),
  ]);
  const statusOptions = [
    { value: '', label: 'All statuses' },
    ...['queued', 'running', 'complete', 'partial', 'failed', 'cancelled', 'skipped'].map(
      (item) => ({ value: item, label: item[0].toUpperCase() + item.slice(1) }),
    ),
  ];
  const kindOptions = [
    { value: '', label: 'Reviews and indexes' },
    { value: 'review', label: 'Reviews only' },
    { value: 'index', label: 'Indexes only' },
  ];
  function clearFilters() {
    repo = '';
    status = '';
    kind = '';
    search = '';
    q = '';
  }
  async function load(more = false) {
    const current = ++generation;
    loading = true;
    try {
      const result = await api.jobs({
        repo,
        status,
        kind,
        q,
        ...(more && next ? { before: next } : {}),
      });
      if (current !== generation) return;
      jobs = more ? [...jobs, ...result.jobs] : result.jobs;
      next = result.next;
      error = '';
    } catch (cause) {
      if (current === generation) error = errorMessage(cause);
    } finally {
      if (current === generation) loading = false;
    }
  }
  onMount(() => {
    readQuery();
    void api
      .repos()
      .then((data) => (repos = data))
      .catch((cause) => (error = errorMessage(cause)));
  });
  function readQuery() {
    const params = page.url.searchParams;
    repo = params.get('repo') || '';
    status = statusOptions.some((item) => item.value === params.get('status'))
      ? params.get('status')!
      : '';
    kind = kindOptions.some((item) => item.value === params.get('kind')) ? params.get('kind')! : '';
    search = q = params.get('q') || '';
    ready = true;
  }
  afterNavigate(readQuery);
  $effect(() => {
    const value = search;
    const timer = setTimeout(() => (q = value), 250);
    return () => clearTimeout(timer);
  });
  $effect(() => {
    const values = { repo, status, kind, q };
    if (!ready) return;
    const url = new URL(window.location.href);
    for (const [key, value] of Object.entries(values)) {
      if (value) url.searchParams.set(key, value);
      else url.searchParams.delete(key);
    }
    if (url.href !== window.location.href) replaceState(url, {});
  });
  $effect(() => {
    repo;
    status;
    kind;
    q;
    $live.job;
    if (ready) void load();
  });
</script>

<svelte:head><title>Reviews · Revera</title></svelte:head>
<div class="page-header">
  <div>
    <h1>Reviews</h1>
    <p>Review history, findings, and run details.</p>
  </div>
  <ExternalReviewDialog {onqueued} />
</div>
{#if queued}<p role="status">
    Private review queued. <a href={`/reviews/${queued}`}>Open review</a>
  </p>{/if}
<ExternalTracking {onqueued} />
<div class="filters">
  <div class="search-field">
    <label class="sr-only" for="review-search">Search reviews</label><Search size={15} />
    <input id="review-search" type="search" bind:value={search} placeholder="Search reviews…" />
  </div>
  <div class="field">
    <label class="sr-only" for="filter-repo">Repository</label><SelectField
      id="filter-repo"
      bind:value={repo}
      options={repoOptions}
    />
  </div>
  <div class="field">
    <label class="sr-only" for="filter-status">Status</label><SelectField
      id="filter-status"
      bind:value={status}
      options={statusOptions}
    />
  </div>
  <div class="field">
    <label class="sr-only" for="filter-kind">Job kind</label><SelectField
      id="filter-kind"
      bind:value={kind}
      options={kindOptions}
    />
  </div>
  <span class="result-count">{jobs.length} {jobs.length === 1 ? 'job' : 'jobs'}</span>
</div>
{#if error}<Notice message={error} retry={() => void load()} />{/if}
{#if jobs.length}<JobsTable {jobs} />{#if next}<div
      class="actions"
      style="justify-content: center; margin-top: 20px"
    >
      <button class="button" disabled={loading} onclick={() => void load(true)}
        >{loading ? 'Loading…' : 'Load more'}</button
      >
    </div>{/if}
{:else if loading}<div class="loading" role="status">Loading reviews…</div>
{:else if !error}<div class="empty-state">
    No jobs match these filters.<br /><button class="button" onclick={clearFilters}
      >Clear filters</button
    >
  </div>{/if}
