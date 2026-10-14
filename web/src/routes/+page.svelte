<script lang="ts">
  import { onMount } from 'svelte';
  import { CheckCircle2, CircleAlert, GitPullRequest, ArrowRight } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import { live } from '#lib/events';
  import { number } from '#lib/time';
  import type { Overview } from '#lib/types';
  import JobsTable from '#lib/components/JobsTable.svelte';
  import Status from '#lib/components/Status.svelte';
  import Notice from '#lib/components/Notice.svelte';

  let overview = $state<Overview>();
  let error = $state('');
  let mounted = false;
  let lastEvent: unknown;
  const attention = $derived(overview ? [
    ...(!overview.health.github.ok ? [overview.health.github.error || 'GitHub needs attention'] : []),
    ...overview.health.models.filter((model) => model.ok !== true).map((model) => model.error || `${model.role} connection needs a test`)
  ] : []);
  async function load() { try { overview = await api.overview(); error = ''; } catch (cause) { error = errorMessage(cause); } }
  onMount(() => { mounted = true; void load(); return () => { mounted = false; }; });
  $effect(() => { const event = $live.job; if (mounted && event && event !== lastEvent) { lastEvent = event; void load(); } });
</script>

<svelte:head><title>Overview · Revera</title></svelte:head>
<div class="page-header"><h1>Overview</h1></div>
{#if error}<Notice message={error} retry={load} />{/if}
{#if overview}
  <div class="health-line" class:needs-attention={attention.length > 0}>
    {#if attention.length}<CircleAlert size={18} /><div><strong>{attention[0]}</strong><p class="helper"><a class="text-link" href="/settings">Check your settings</a></p></div>{:else}<CheckCircle2 size={18} /><strong>Everything is connected</strong>{/if}
    <div class="health-details"><Status status={overview.health.github.ok ? 'complete' : 'failed'} label="GitHub" /><Status status={overview.health.models.every((model) => model.ok) ? 'complete' : 'partial'} label="Models" />{#if overview.health.vera.enabled}<Status status="complete" label="Vera" />{/if}</div>
  </div>
  <section class="metrics" aria-labelledby="metrics-title">
    <h2 id="metrics-title" class="metrics-title">Last 24 hours</h2>
    <div class="stats">{#each [{ label: 'Reviews', value: overview.last_24h.reviews }, { label: 'Validated findings', value: overview.last_24h.findings }, { label: 'Failed reviews', value: overview.last_24h.failed }, { label: 'Tokens used', value: overview.last_24h.tokens }] as stat}<div class="stat"><div class="stat-label">{stat.label}</div><div class="stat-value tabular">{number(stat.value)}</div></div>{/each}</div>
  </section>
  <section class="section" aria-labelledby="queue-title">
    <div class="section-heading"><h2 id="queue-title">In progress <span class="muted" style="font-size: 12px; font-weight: 400; margin-left: 8px">{overview.queue.running.length} running · {overview.queue.queued.length} queued</span></h2><span class="live-label" class:offline={!$live.connected}>{$live.connected ? 'Live updates' : 'Reconnecting'}</span></div>
    {#if overview.queue.running.length + overview.queue.queued.length}
      <div class="queue-list">{#each [...overview.queue.running, ...overview.queue.queued] as job}<div class="queue-row"><GitPullRequest size={18} /><div class="queue-description"><a class="row-title" href={`/reviews/${job.id}`}>{job.title || 'Repository index'}</a><div class="row-meta">{job.repo}<span>·</span>{job.pr ? `#${job.pr}` : 'Index'}</div></div><Status status={job.status} /><a href={`/reviews/${job.id}`} class="button ghost small" aria-label={`View ${job.title || 'index'}`}><ArrowRight size={16} /></a></div>{/each}</div>
    {:else}<div class="empty-state">No jobs are waiting. <a class="text-link" href="/repositories">Review a pull request</a></div>{/if}
  </section>
  <section class="section" aria-labelledby="recent-title"><div class="section-heading"><h2 id="recent-title">Recent reviews</h2><a class="button ghost small" href="/reviews">View all <ArrowRight size={14} /></a></div>{#if overview.recent.length}<JobsTable jobs={overview.recent} />{:else}<div class="empty-state">Your first review will appear here.<br /><a class="button" href="/repositories">Choose a repository</a></div>{/if}</section>
{:else if !error}<div class="loading" role="status">Loading overview…</div>{/if}
