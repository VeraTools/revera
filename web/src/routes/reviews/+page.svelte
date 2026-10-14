<script lang="ts">
  import { onMount } from 'svelte';
  import { api, errorMessage } from '#lib/api';
  import { live } from '#lib/events';
  import type { JobSummary, Repository } from '#lib/types';
  import JobsTable from '#lib/components/JobsTable.svelte';
  import Notice from '#lib/components/Notice.svelte';
  import SelectField from '#lib/components/SelectField.svelte';
  let jobs = $state<JobSummary[]>([]);
  let repos = $state<Repository[]>([]);
  let repo = $state(''); let status = $state(''); let kind = $state('');
  let next = $state<string>(); let error = $state(''); let loading = $state(true);
  let ready = $state(false); let generation = 0;
  async function load(more = false) {
    const current = ++generation;
    loading = true;
    try {
      const result = await api.jobs({ repo, status, kind, ...(more && next ? { before: next } : {}) });
      if (current !== generation) return;
      jobs = more ? [...jobs, ...result.jobs] : result.jobs; next = result.next; error = '';
    } catch (cause) { if (current === generation) error = errorMessage(cause); } finally { if (current === generation) loading = false; }
  }
  onMount(() => { void api.repos().then((data) => repos = data).catch((cause) => error = errorMessage(cause)); ready = true; });
  $effect(() => { repo; status; kind; $live.job; if (ready) void load(); });
</script>

<svelte:head><title>Reviews · Revera</title></svelte:head>
<div class="page-header"><div><h1>Reviews</h1><p>Review history, findings, and run details.</p></div></div>
<div class="filters">
  <div class="field"><label class="sr-only" for="filter-repo">Repository</label><SelectField id="filter-repo" bind:value={repo} options={[{ value: '', label: 'All repositories' }, ...repos.map((item) => ({ value: item.full_name, label: item.full_name }))]} /></div>
  <div class="field"><label class="sr-only" for="filter-status">Status</label><SelectField id="filter-status" bind:value={status} options={[{ value: '', label: 'All statuses' }, ...['queued', 'running', 'complete', 'partial', 'failed', 'cancelled', 'skipped'].map((item) => ({ value: item, label: item[0].toUpperCase() + item.slice(1) }))]} /></div>
  <div class="field"><label class="sr-only" for="filter-kind">Job kind</label><SelectField id="filter-kind" bind:value={kind} options={[{ value: '', label: 'Reviews and indexes' }, { value: 'review', label: 'Reviews only' }, { value: 'index', label: 'Indexes only' }]} /></div>
  <span class="result-count">{jobs.length} {jobs.length === 1 ? 'job' : 'jobs'}</span>
</div>
{#if error}<Notice message={error} retry={() => void load()} />{/if}
{#if jobs.length}<JobsTable {jobs} />{#if next}<div class="actions" style="justify-content: center; margin-top: 20px"><button class="button" disabled={loading} onclick={() => void load(true)}>{loading ? 'Loading…' : 'Load more'}</button></div>{/if}
{:else if loading}<div class="loading" role="status">Loading reviews…</div>
{:else if !error}<div class="empty-state">No jobs match these filters.<br /><button class="button" onclick={() => { repo = ''; status = ''; kind = ''; }}>Clear filters</button></div>{/if}
