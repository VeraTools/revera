<script lang="ts">
  import { page } from '$app/state';
  import { live } from '#lib/events';
  import { ArrowLeft, ArrowUpRight, RotateCcw, Square, ChevronDown, Copy } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import { duration, number } from '#lib/time';
  import type { JobDetail } from '#lib/types';
  import Status from '#lib/components/Status.svelte';
  import Notice from '#lib/components/Notice.svelte';
  let detail = $state<JobDetail>();
  let log = $state('');
  let error = $state('');
  let message = $state('');
  let busy = $state(false);
  let showLog = $state(false);
  let generation = 0;
  async function load(id: string) {
    const current = ++generation;
    try {
      const result = await api.job(id);
      if (current === generation) {
        detail = result;
        error = '';
      }
    } catch (cause) {
      if (current === generation) error = errorMessage(cause);
    }
  }
  $effect(() => {
    const id = page.params.id;
    if (id) {
      showLog = false;
      log = '';
      void load(id);
    }
  });
  $effect(() => {
    if ($live.job?.id === page.params.id) void load(page.params.id!);
  });
  async function action(which: 'rerun' | 'cancel') {
    if (!detail) return;
    busy = true;
    try {
      await api[which](detail.job.id);
      message = which === 'rerun' ? 'A new review has been queued.' : 'The job has been cancelled.';
      await load(detail.job.id);
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
  async function loadLog() {
    if (!detail) return;
    try {
      log = await api.log(detail.job.id);
      showLog = true;
    } catch (cause) {
      error = errorMessage(cause);
    }
  }
  async function copyLog() {
    try {
      await navigator.clipboard.writeText(log);
      message = 'Log copied.';
    } catch {
      error = 'Could not copy the log. Select the text and copy it manually.';
    }
  }
</script>

<svelte:head><title>Review details · Revera</title></svelte:head>
<a class="back-link" href="/reviews"><ArrowLeft size={14} />All reviews</a>
{#if error}<Notice message={error} retry={() => void load(page.params.id!)} />{/if}
{#if message}<Notice {message} kind="success" />{/if}
{#if detail}
  <div class="detail-header">
    <Status status={detail.job.status} />
    <h1>{detail.job.title || 'Repository index'}</h1>
    <div class="row-meta">
      <span>{detail.job.repo}</span>{#if detail.job.pr}<span>·</span><span>#{detail.job.pr}</span
        >{/if}{#if detail.job.author}<span>·</span><span>Opened by {detail.job.author}</span>{/if}
    </div>
  </div>
  <div class="detail-layout">
    <div>
      <h2>Summary</h2>
      <p class="detail-summary">
        {detail.summary_markdown || detail.job.reason || 'No summary is available yet.'}
      </p>
      <div class="section-heading">
        <h2>
          Findings <span class="muted" style="font-weight: 400; margin-left: 8px; font-size: 12px"
            >{detail.findings.length}</span
          >
        </h2>
        <span class="helper">Checked independently</span>
      </div>
      {#each detail.findings as finding, index}<details class="finding" open={index === 0}>
          <summary
            ><div class="finding-top">
              <Status status={finding.severity} /><span class="mono"
                >{finding.file}{finding.line ? `:${finding.line}` : ''}</span
              >
            </div>
            <div class="finding-title">{finding.title}<ChevronDown size={15} /></div></summary
          >
          <p class="finding-body">{finding.body || 'No additional detail.'}</p>
        </details>{:else}<p class="muted" style="font-size: 13px; padding: 20px 0">
          {['queued', 'running'].includes(detail.job.status)
            ? 'Findings will appear after validation.'
            : 'No findings were reported.'}
        </p>{/each}
      <section class="section">
        <div class="section-heading">
          <h2>Run log</h2>
          <div class="actions">
            {#if showLog}<button class="button small" onclick={copyLog}
                ><Copy size={13} />Copy</button
              ><button class="button small" onclick={loadLog}>Refresh</button>{:else}<button
                class="button small"
                onclick={loadLog}>View log</button
              >{/if}
          </div>
        </div>
        {#if showLog}<textarea
            class="log-viewer mono"
            aria-label="Run log output"
            readonly
            rows="7"
            value={log || 'No log output yet.'}></textarea>{:else}<p class="helper">
            Process output, with secrets removed.
          </p>{/if}
      </section>
      {#if detail.ledger}<details class="data-disclosure">
          <summary>Request ledger</summary>
          <pre>{JSON.stringify(detail.ledger, null, 2)}</pre>
        </details>{/if}
    </div>
    <aside class="detail-meta" aria-label="Run details">
      <h3>Run details</h3>
      <dl>
        <div>
          <dt>Duration</dt>
          <dd class="tabular">{duration(detail.job.duration_ms)}</dd>
        </div>
        <div>
          <dt>Triggered by</dt>
          <dd style="text-transform: capitalize">{detail.job.trigger}</dd>
        </div>
        <div>
          <dt>Commit</dt>
          <dd class="mono" title={detail.job.head_sha}>
            {detail.job.head_sha?.slice(0, 8) || '—'}
          </dd>
        </div>
        <div>
          <dt>Started</dt>
          <dd>
            {detail.job.started_at
              ? new Date(detail.job.started_at).toLocaleString()
              : 'Waiting in queue'}
          </dd>
        </div>
      </dl>
      {#if detail.job.tokens}<h3>Token use</h3>
        <dl>
          <div>
            <dt>Prompt</dt>
            <dd class="tabular">{number(detail.job.tokens.prompt)}</dd>
          </div>
          <div>
            <dt>Completion</dt>
            <dd class="tabular">{number(detail.job.tokens.completion)}</dd>
          </div>
          <div>
            <dt>Cached</dt>
            <dd class="tabular">{number(detail.job.tokens.cached)}</dd>
          </div>
        </dl>{/if}
      {#if detail.timing}<details class="data-disclosure" style="margin-bottom: 24px">
          <summary>Stage timing</summary>
          <pre>{JSON.stringify(detail.timing, null, 2)}</pre>
        </details>{/if}
      <div class="actions">
        {#if detail.job.pr_url}<a
            class="button"
            href={detail.job.pr_url}
            target="_blank"
            rel="noreferrer">Pull request<ArrowUpRight size={14} /></a
          >{/if}{#if detail.job.check_run_url}<a
            class="button"
            href={detail.job.check_run_url}
            target="_blank"
            rel="noreferrer">GitHub check<ArrowUpRight size={14} /></a
          >{/if}{#if ['queued', 'running'].includes(detail.job.status)}<button
            class="button danger"
            disabled={busy}
            onclick={() => void action('cancel')}>Cancel job<Square size={13} /></button
          >{:else if detail.job.kind === 'review'}<button
            class="button"
            disabled={busy}
            onclick={() => void action('rerun')}>Re-run review<RotateCcw size={14} /></button
          >{/if}
      </div>
    </aside>
  </div>
{:else if !error}<div class="loading" role="status">Loading review…</div>{/if}
