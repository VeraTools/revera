<script lang="ts">
  import { onMount } from 'svelte';
  import { CheckCircle2, CircleAlert, GitPullRequest, ArrowRight } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import { live } from '#lib/events';
  import { duration, number, relative, severitySplit } from '#lib/time';
  import type { Overview } from '#lib/types';
  import { triggerLabel } from '#lib/job-labels';
  import JobsTable from '#lib/components/JobsTable.svelte';
  import Status from '#lib/components/Status.svelte';
  import Notice from '#lib/components/Notice.svelte';

  let overview = $state<Overview>();
  let period = $state('24h');
  let ready = $state(false);
  let error = $state('');
  let mounted = false;
  let lastEvent: unknown;
  const windowStats = $derived(period === '7d' ? overview?.last_7d : overview?.last_24h);
  const partialHistory = $derived(
    !!overview?.history_since &&
      Date.parse(overview.history_since) > Date.now() - (period === '7d' ? 7 : 1) * 86400000,
  );
  const stats = $derived(
    windowStats
      ? [
          {
            label: 'Reviews',
            value: number(windowStats.reviews),
            caption: windowStats.failed ? `${windowStats.failed} failed` : 'none failed',
          },
          {
            label: 'Validated findings',
            value: number(windowStats.findings),
            caption: severitySplit(windowStats.severity) || 'none',
          },
          {
            label: 'Median review time',
            value: duration(windowStats.median_duration_ms ?? undefined),
            caption: 'per review',
          },
          {
            label: 'Tokens used',
            value: number(windowStats.tokens),
            caption: windowStats.prompt_tokens
              ? `${Math.round((100 * windowStats.cached_tokens) / windowStats.prompt_tokens)}% cached`
              : '',
          },
        ]
      : [],
  );
  const attention = $derived(
    overview
      ? [
          ...(!overview.health.github.ok
            ? [overview.health.github.error || 'GitHub needs attention']
            : []),
          ...overview.health.models
            .filter((model) => model.ok !== true)
            .map((model) => model.error || `${model.role} connection needs a test`),
        ]
      : [],
  );
  async function load() {
    try {
      overview = await api.overview();
      error = '';
    } catch (cause) {
      error = errorMessage(cause);
    }
  }
  onMount(() => {
    try {
      period = localStorage.getItem('revera-overview-period') === '7d' ? '7d' : '24h';
    } catch {
      /* Storage may be disabled. */
    }
    ready = true;
    mounted = true;
    void load();
    return () => {
      mounted = false;
    };
  });
  $effect(() => {
    if (ready)
      try {
        localStorage.setItem('revera-overview-period', period);
      } catch {
        /* Storage may be disabled. */
      }
  });
  $effect(() => {
    const event = $live.job;
    if (mounted && event && event !== lastEvent) {
      lastEvent = event;
      void load();
    }
  });
</script>

<svelte:head><title>Overview · Revera</title></svelte:head>
<div class="page-header"><h1>Overview</h1></div>
{#if error}<Notice message={error} retry={load} />{/if}
{#if overview}
  <div class="health-line" class:needs-attention={attention.length > 0}>
    {#if attention.length}<CircleAlert size={18} />
      <div>
        <strong>{attention[0]}</strong>
        <p class="helper"><a class="text-link" href="/settings">Check your settings</a></p>
      </div>{:else}<CheckCircle2 size={18} /><strong>Everything is connected</strong>{/if}
    <div class="health-details">
      <Status status={overview.health.github.ok ? 'complete' : 'failed'} label="GitHub" />
      {#each overview.health.models as model}<span
          title={model.error ||
            (model.checked_at ? `Checked ${relative(model.checked_at)}` : 'Not tested yet')}
        >
          <Status
            status={model.ok === true
              ? 'complete'
              : model.ok === false || model.error
                ? 'failed'
                : 'partial'}
            label={`${model.role[0].toUpperCase() + model.role.slice(1)} · ${model.model}`}
          />
        </span>{/each}
      {#if overview.health.vera.enabled}<Status
          status="complete"
          label={`Vera${overview.health.vera.version ? ` ${overview.health.vera.version}` : ''}`}
        />{/if}
    </div>
  </div>
  {#if overview.app.issue_comment_subscribed === false}
    <section class="mention-notice" aria-labelledby="mention-notice-title">
      <h2 id="mention-notice-title">Mentions need one change on your GitHub App</h2>
      <ol>
        <li>Set Issues to Read-only in Permissions &amp; events.</li>
        <li>Subscribe to Issue comment and save.</li>
        <li>Accept the new permissions on the installation.</li>
      </ol>
      <a class="text-link" href={overview.app.settings_url} target="_blank" rel="noreferrer"
        >Open GitHub App settings <ArrowRight size={13} /></a
      >
    </section>
  {/if}
  <section class="metrics" aria-labelledby="metrics-title">
    <div class="section-heading metrics-heading">
      <h2 id="metrics-title" class="metrics-title">Activity</h2>
      <div class="segmented" role="group" aria-label="Overview period">
        <button aria-pressed={period === '24h'} onclick={() => (period = '24h')}>24 hours</button>
        <button aria-pressed={period === '7d'} onclick={() => (period = '7d')}>7 days</button>
      </div>
    </div>
    {#if partialHistory}<p class="helper history-note">
        Showing jobs since {new Date(overview.history_since!).toLocaleDateString()}. History keeps
        the last 500 jobs.
      </p>{/if}
    <div class="stats">
      {#each stats as stat}<div class="stat">
          <div class="stat-label">{stat.label}</div>
          <div class="stat-value tabular">{stat.value}</div>
          {#if stat.caption}<div class="stat-caption">{stat.caption}</div>{/if}
        </div>{/each}
    </div>
  </section>
  <section class="section" aria-labelledby="queue-title">
    <div class="section-heading">
      <h2 id="queue-title">
        In progress <span class="muted" style="font-size: 12px; font-weight: 400; margin-left: 8px"
          >{overview.queue.running.length} running · {overview.queue.queued.length} queued</span
        >
      </h2>
      <span class="live-label" class:offline={!$live.connected}
        >{$live.connected ? 'Live updates' : 'Reconnecting'}</span
      >
    </div>
    {#if overview.queue.running.length + overview.queue.queued.length}
      <div class="queue-list">
        {#each [...overview.queue.running, ...overview.queue.queued] as job}<div class="queue-row">
            <GitPullRequest size={18} />
            <div class="queue-description">
              <a class="row-title" href={`/reviews/${job.id}`}>{job.title || 'Repository index'}</a>
              <div class="row-meta">
                {job.repo}<span>·</span>{job.pr ? `#${job.pr}` : 'Index'}
                {#if job.kind === 'review'}<span>·</span><span class="capitalize">{job.mode}</span
                  >{/if}<span>·</span>{triggerLabel(job.trigger)}
                <span>·</span>{job.status === 'running' && job.started_at
                  ? `started ${relative(job.started_at).toLowerCase()}`
                  : `queued ${relative(job.queued_at).toLowerCase()}`}
              </div>
            </div>
            <Status status={job.status} /><a
              href={`/reviews/${job.id}`}
              class="button ghost small"
              aria-label={`View ${job.title || 'index'}`}><ArrowRight size={16} /></a
            >
          </div>{/each}
      </div>
    {:else}<div class="empty-state">
        No jobs are waiting. <a class="text-link" href="/repositories">Review a pull request</a>
      </div>{/if}
  </section>
  <section class="section" aria-labelledby="recent-title">
    <div class="section-heading">
      <h2 id="recent-title">Recent reviews</h2>
      <a class="button ghost small" href="/reviews">View all <ArrowRight size={14} /></a>
    </div>
    {#if overview.recent.length}<JobsTable jobs={overview.recent} />{:else}<div class="empty-state">
        Your first review will appear here.<br /><a class="button" href="/repositories"
          >Choose a repository</a
        >
      </div>{/if}
  </section>
{:else if !error}<div class="loading" role="status">Loading overview…</div>{/if}

<style>
  .mention-notice {
    padding: 20px 24px;
    margin: 24px 0;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .mention-notice h2 {
    font-size: 14px;
  }
  ol {
    margin: 12px 0;
    padding-left: 18px;
    color: var(--muted);
    font-size: 12px;
    line-height: 1.9;
    list-style: decimal;
  }
  .mention-notice .text-link {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
</style>
