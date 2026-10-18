<script lang="ts">
  import { untrack } from 'svelte';
  import { page } from '$app/state';
  import { live } from '#lib/events';
  import { ArrowLeft, ArrowUpRight, RotateCcw, Square, ChevronDown, Copy } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import { duration, number, severitySplit } from '#lib/time';
  import { renderSummary } from '#lib/markdown';
  import { createNotices } from '#lib/notices.svelte';
  import type { JobDetail, ReviewMode, Repository } from '#lib/types';
  import ReviewModeField from '#lib/components/ReviewModeField.svelte';
  import Status from '#lib/components/Status.svelte';
  import Notice from '#lib/components/Notice.svelte';
  let detail = $state<JobDetail>();
  let log = $state('');
  const notice = createNotices();
  let busy = $state(false);
  let showLog = $state(false);
  let showDiagnostics = $state(false);
  let generation = 0;
  let repository = $state<Repository>();
  let reviewMode = $state<ReviewMode | ''>('');
  let now = $state(Date.now());
  const elapsed = $derived(
    detail?.job.status === 'running' && detail.job.started_at
      ? Math.max(0, now - Date.parse(detail.job.started_at))
      : detail?.job.duration_ms,
  );
  const split = $derived(severitySplit(detail?.counts ?? { high: 0, medium: 0, low: 0 }));
  const timingRows = $derived(
    [
      { label: 'Total', value: detail?.timing?.total_ms },
      { label: 'Index', value: detail?.timing?.vera_index_ms },
      { label: 'Investigation', value: detail?.timing?.lanes_ms },
      { label: 'Validation', value: detail?.timing?.validate_ms },
    ].filter((row) => row.value !== undefined && row.value !== null),
  );
  const reviewRows = $derived(
    [
      { label: 'Candidates', value: detail?.stats?.candidates },
      { label: 'Validated', value: detail?.stats?.accepted },
      { label: 'Rejected', value: detail?.stats?.rejected },
      { label: 'Files read', value: detail?.stats?.files_read },
      { label: 'Retrieval', value: detail?.stats?.retrieval },
    ].filter((row) => row.value !== undefined && row.value !== null),
  );
  const integer = (value?: number) => (value ?? 0).toLocaleString('en');
  $effect(() => {
    if (detail?.job.status !== 'running') return;
    now = Date.now();
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });
  const reasons: Record<string, string> = {
    rate_limit_or_server: 'rate limit or server error',
    authentication: 'key rejected',
    not_found: 'model not found',
    timeout: 'request timed out',
    transport: 'connection failed',
    malformed_response: 'invalid response',
    missing_key: 'key not set',
  };
  const roleLabel = (role: string) => role[0].toUpperCase() + role.slice(1);
  async function loadRepository(repo: string) {
    try {
      const match = (await api.repos()).find((item) => item.full_name === repo);
      if (detail?.job.repo === repo) repository = match;
    } catch (cause) {
      if (detail?.job.repo === repo) notice.failure(errorMessage(cause));
    }
  }
  async function load(id: string) {
    const current = ++generation;
    try {
      const result = await api.job(id, showDiagnostics);
      if (current === generation) {
        if (detail?.job.id !== result.job.id) {
          repository = undefined;
          reviewMode = '';
          void loadRepository(result.job.repo);
        }
        detail = result;
        notice.state.error = '';
      }
    } catch (cause) {
      if (current === generation) notice.failure(errorMessage(cause));
    }
  }
  $effect(() => {
    const id = page.params.id;
    if (id) {
      showLog = false;
      showDiagnostics = false;
      log = '';
      untrack(() => void load(id));
    }
  });
  $effect(() => {
    if ($live.job?.id === page.params.id) void load(page.params.id!);
  });
  async function action(which: 'rerun' | 'cancel') {
    if (!detail) return;
    busy = true;
    try {
      if (which === 'rerun') await api.rerun(detail.job.id, reviewMode || null);
      else await api.cancel(detail.job.id);
      notice.success(
        which === 'rerun' ? 'A new review has been queued.' : 'The job has been cancelled.',
      );
      await load(detail.job.id);
    } catch (cause) {
      notice.failure(errorMessage(cause));
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
      notice.failure(errorMessage(cause));
    }
  }
  async function copyLog() {
    try {
      await navigator.clipboard.writeText(log);
      notice.success('Log copied.');
    } catch {
      notice.failure('Could not copy the log. Select the text and copy it manually.');
    }
  }
  async function copyInstructions(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      notice.success('Fix instructions copied.');
    } catch {
      notice.failure('Could not copy instructions. Download the .md file instead.');
    }
  }
  function downloadInstructions() {
    if (!detail) return;
    const url = URL.createObjectURL(
      new Blob([detail.instructions], { type: 'text/markdown;charset=utf-8' }),
    );
    const link = document.createElement('a');
    link.href = url;
    link.download = 'revera-instructions.md';
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  async function toggleDiagnostics() {
    showDiagnostics = !showDiagnostics;
    if (detail) await load(detail.job.id);
  }
</script>

<svelte:head><title>Review details · Revera</title></svelte:head>
<a class="back-link" href="/reviews"><ArrowLeft size={14} />All reviews</a>
{#if notice.state.error}<Notice
    message={notice.state.error}
    retry={() => void load(page.params.id!)}
  />{/if}
{#if notice.state.message}<Notice message={notice.state.message} kind="success" />{/if}
{#if detail}
  <div class="detail-header">
    <Status status={detail.job.status} />
    <h1>{detail.job.title || 'Repository index'}</h1>
    <div class="row-meta">
      <span>{detail.job.repo}</span>{#if detail.job.pr}<span>·</span><span>#{detail.job.pr}</span
        >{/if}{#if detail.job.author}<span>·</span><span>Opened by {detail.job.author}</span>{/if}
    </div>
    {#if detail.reviewed_head_sha}<p class="helper">
        Reviewed SHA <code class="mono" title={detail.reviewed_head_sha}
          >{detail.reviewed_head_sha.slice(0, 12)}</code
        >
        {#if detail.stale === true}<span class="stale-badge">Potentially stale</span>
        {:else if detail.stale === null}<span> · Freshness unknown</span>{/if}
      </p>{/if}
  </div>
  <div class="detail-layout">
    <div>
      <h2>Summary</h2>
      {#if ['partial', 'failed', 'skipped', 'cancelled'].includes(detail.job.status)}
        <p class="route-notes" role="note">
          Review {detail.job.status}. This is not a clean result. {detail.job.reason ||
            'The review did not complete.'}
        </p>
      {/if}
      {#if detail.summary_markdown}<div class="summary">
          {@html renderSummary(detail.summary_markdown)}
        </div>
      {:else}<p class="detail-summary">
          {detail.job.reason || 'No summary is available yet.'}
        </p>{/if}
      {#if detail.ledger?.fallback_events?.length || detail.ledger?.unavailable_routes?.length}
        <section class="route-notes" aria-label="Model changes">
          <h2>Model changes</h2>
          {#each detail.ledger.fallback_events || [] as event}
            <p>
              {roleLabel(event.role)} switched from {event.from_model} to {event.to_model}: {reasons[
                event.reason
              ] || event.reason}.
            </p>
          {/each}
          {#each detail.ledger.unavailable_routes || [] as route}
            <p>
              {roleLabel(route.role)} skipped {route.model}: {reasons[route.reason] ||
                route.reason}.
            </p>
          {/each}
        </section>
      {/if}
      <div class="section-heading">
        <h2>
          Accepted findings <span
            class="muted"
            style="font-weight: 400; margin-left: 8px; font-size: 12px"
            >{detail.findings.length}{#if split}{' · '}{split}{/if}</span
          >
        </h2>
        <span class="helper">Checked independently</span>
      </div>
      {#if detail.job.status === 'complete' && detail.findings.length}
        <div class="actions handoff-actions">
          <button class="button small" onclick={() => copyInstructions(detail!.instructions)}
            ><Copy size={13} />Copy fix instructions</button
          >
          <button class="button small" onclick={downloadInstructions}>Download .md</button>
        </div>
      {/if}
      {#each detail.findings as finding, index}<details class="finding" open={index === 0}>
          <summary
            ><div class="finding-top">
              <Status status={finding.severity} />
              {#if finding.html_url}<a
                  class="mono text-link"
                  href={finding.html_url}
                  target="_blank"
                  rel="noreferrer"
                  >{finding.file}{finding.start_line
                    ? `:${finding.start_line}${finding.end_line > finding.start_line ? `-${finding.end_line}` : ''}`
                    : ''}</a
                >
              {:else}<span class="mono"
                  >{finding.file}{finding.start_line ? `:${finding.start_line}` : ''}</span
                >{/if}
            </div>
            <div class="finding-title">{finding.title}<ChevronDown size={15} /></div></summary
          >
          <p class="helper">Finding <code>{finding.id}</code></p>
          <p class="finding-body">{finding.claim || 'No additional detail.'}</p>
          {#if finding.trigger}<p class="finding-body">
              <strong>Trigger:</strong>
              {finding.trigger}
            </p>{/if}
          {#if finding.impact}<p class="finding-body">
              <strong>Impact:</strong>
              {finding.impact}
            </p>{/if}
          {#if finding.supporting_evidence.length}<h3 class="helper">Supporting evidence</h3>
            <ul class="finding-evidence">
              {#each finding.supporting_evidence as evidence}<li>
                  <code>{evidence.path}:{evidence.start_line}-{evidence.end_line}</code> — {evidence.note}
                </li>{/each}
            </ul>
          {/if}
          {#if finding.validated_fix}<p class="helper">{finding.validated_fix_label}</p>
            <p class="finding-body">{finding.validated_fix}</p>{/if}
          {#if finding.suggestion}<p class="helper">Exact suggested change — not a tested patch</p>
            <pre class="finding-replacement">{finding.suggestion.replacement}</pre>{/if}
          <button class="button small" onclick={() => copyInstructions(finding.prompt)}
            ><Copy size={13} />Copy prompt</button
          >
        </details>{:else}<p class="muted" style="font-size: 13px; padding: 20px 0">
          {['queued', 'running'].includes(detail.job.status)
            ? 'Findings will appear after validation.'
            : detail.job.status === 'complete'
              ? 'No accepted findings in this review.'
              : 'No accepted findings. The review did not complete; this is not a clean result.'}
        </p>{/each}
      <section class="section" aria-label="Diagnostics">
        <button class="button small" aria-expanded={showDiagnostics} onclick={toggleDiagnostics}
          >{showDiagnostics ? 'Hide diagnostics' : 'Show diagnostics'}</button
        >
        {#if showDiagnostics}<h3 class="helper">Not validated — not reported as defects</h3>
          {#each detail.diagnostics || [] as finding}<article class="diagnostic">
              <p><strong>{finding.status}</strong> · {finding.title}</p>
              <p class="helper">{finding.file}:{finding.start_line || '—'} — {finding.rationale}</p>
            </article>{:else}<p class="helper">No diagnostics.</p>{/each}
        {/if}
      </section>
      {#if detail.ledger?.by_route?.length}<section class="section" aria-labelledby="usage-title">
          <div class="section-heading"><h2 id="usage-title">Model usage</h2></div>
          <div class="table-scroll">
            <table class="model-usage">
              <thead
                ><tr
                  ><th>Role</th><th>Model</th><th>Requests</th><th>Prompt</th><th>Completion</th><th
                    >Cached</th
                  ></tr
                ></thead
              >
              <tbody
                >{#each detail.ledger.by_route as route}<tr
                    ><td style="text-transform: capitalize">{route.role}</td>
                    <td
                      >{route.model}{#if route.effective_reasoning}<span class="muted"
                          >@{route.effective_reasoning}</span
                        >{/if}</td
                    >
                    <td class="tabular">{integer(route.requests)}</td><td class="tabular"
                      >{integer(route.prompt_tokens)}</td
                    >
                    <td class="tabular">{integer(route.completion_tokens)}</td><td class="tabular"
                      >{integer(route.cached_prompt_tokens)}</td
                    >
                  </tr>{/each}</tbody
              >
            </table>
          </div>
        </section>{/if}
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
      {#if detail.ledger || detail.timing || detail.stats}<details class="data-disclosure">
          <summary>Raw report data</summary>
          <pre>{JSON.stringify(
              { ledger: detail.ledger, timing: detail.timing, stats: detail.stats },
              null,
              2,
            )}</pre>
        </details>{/if}
    </div>
    <aside class="detail-meta" aria-label="Run details">
      <h3>Run details</h3>
      <dl>
        {#if detail.job.kind === 'review'}<div>
            <dt>Mode</dt>
            <dd style="text-transform: capitalize">{detail.job.mode}</dd>
          </div>{/if}
        {#if detail.strategy}<div>
            <dt>Strategy</dt>
            <dd>{detail.strategy}</dd>
          </div>{/if}
        <div>
          <dt>Duration</dt>
          <dd class="tabular">
            {duration(elapsed)}{#if detail.job.status === 'running'}
              <span class="muted">elapsed</span>{/if}
          </dd>
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
            <dd class="tabular">
              {number(detail.job.tokens.cached)}{#if detail.job.tokens.prompt}{' '}
                ({Math.round((100 * detail.job.tokens.cached) / detail.job.tokens.prompt)}%){/if}
            </dd>
          </div>
        </dl>{/if}
      {#if timingRows.length}<h3>Timing</h3>
        <dl class="compact-details">
          {#each timingRows as row}<div>
              <dt>{row.label}</dt>
              <dd class="tabular">
                {duration(row.value)}
                {#if row.label === 'Validation' && detail.timing?.validate_p95_ms !== undefined}<span
                    class="muted"
                  >
                    · p95 {duration(detail.timing.validate_p95_ms)}</span
                  >{/if}
              </dd>
            </div>{/each}
        </dl>{/if}
      {#if reviewRows.length}<h3>Review</h3>
        <dl class="compact-details">
          {#each reviewRows as row}<div>
              <dt>{row.label}</dt>
              <dd>{row.value}</dd>
            </div>{/each}
        </dl>{/if}
      {#if detail.job.kind === 'review' && !['queued', 'running'].includes(detail.job.status)}
        <div style="margin-bottom: 16px">
          <ReviewModeField
            id="rerun-mode"
            bind:value={reviewMode}
            defaultMode={repository?.mode || 'standard'}
          />
          {#if repository?.trigger === 'off'}<p class="helper">
              Reviews are off for this repository.
            </p>{/if}
        </div>
      {/if}
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
            disabled={busy || !repository || repository.trigger === 'off'}
            onclick={() => void action('rerun')}>Re-run review<RotateCcw size={14} /></button
          >{/if}
      </div>
    </aside>
  </div>
{:else if !notice.state.error}<div class="loading" role="status">Loading review…</div>{/if}

<style>
  .detail-layout > div {
    min-width: 0;
  }
  .handoff-actions {
    margin-bottom: 16px;
    flex-wrap: wrap;
  }
  .stale-badge {
    margin-left: 8px;
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 2px 6px;
  }
  .finding-evidence,
  .diagnostic {
    font-size: 12px;
    line-height: 1.8;
    margin: 12px 0;
    overflow-wrap: anywhere;
  }
  .finding-replacement {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: 12px;
    padding: 12px;
    border: 1px solid var(--border);
  }
  .summary {
    margin: 16px 0 28px;
    font-size: 13px;
    line-height: 1.8;
    overflow-wrap: anywhere;
  }
  .summary :global(p),
  .summary :global(ul),
  .summary :global(blockquote) {
    margin: 12px 0;
  }
  .summary :global(h3) {
    font-size: 14px;
    margin: 20px 0 8px;
  }
  .summary :global(h4) {
    font-size: 13px;
    font-weight: 600;
    margin: 20px 0 8px;
  }
  .summary :global(ul) {
    list-style: disc;
    padding-left: 20px;
  }
  .summary :global(li) {
    margin: 5px 0;
  }
  .summary :global(code) {
    font-size: 11px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 1px 4px;
  }
  .summary :global(blockquote) {
    border-left: 2px solid var(--strong-border);
    padding-left: 14px;
    color: var(--muted);
  }
  .summary :global(a) {
    text-decoration: underline;
    text-underline-offset: 3px;
  }
  .summary :global(.summary-footer) {
    font-size: 11px;
    color: var(--muted);
  }
  .model-usage {
    min-width: 560px;
  }
  .model-usage :global(th),
  .model-usage td {
    padding: 10px 12px;
    font-size: 11px;
  }
  .route-notes {
    margin: 0 0 28px;
    padding: 16px 20px;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .route-notes h2 {
    font-size: 13px;
    margin-bottom: 8px;
  }
  .route-notes p {
    font-size: 12px;
    color: var(--muted);
    line-height: 1.8;
    overflow-wrap: anywhere;
  }
</style>
