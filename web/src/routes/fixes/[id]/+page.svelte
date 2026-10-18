<script lang="ts">
  import { untrack } from 'svelte';
  import { page } from '$app/state';
  import { api, errorMessage } from '#lib/api';
  import { live } from '#lib/events';
  import type { FixDetail, FixDelivery as Delivery, FixAction } from '#lib/types';
  import FixStatus from '#lib/components/FixStatus.svelte';
  import FixDelivery from '#lib/components/FixDelivery.svelte';
  import FixDiff from '#lib/components/FixDiff.svelte';
  import Status from '#lib/components/Status.svelte';
  import Notice from '#lib/components/Notice.svelte';
  let detail = $state<FixDetail>();
  let delivery = $state<Delivery>('patch');
  let error = $state('');
  let message = $state('');
  let busy = $state('');
  let note = $state('');
  let once = $state(false);
  let executionEnabled = $state(false);
  let generation = 0;
  const running = $derived(
    !!detail &&
      ['queued', 'generating', 'validating', 'publishing', 'verifying'].includes(detail.fix.state),
  );
  const canPublish = $derived(
    detail?.actions.approve &&
      delivery !== 'patch' &&
      detail.delivery.some((c) => c.mode === delivery && c.available),
  );
  const publishReason = $derived(
    detail?.reasons.approve ||
      (delivery === 'patch'
        ? 'Patch preview does not write to GitHub. Choose an available write destination to publish.'
        : detail?.delivery.find((c) => c.mode === delivery)?.reason || ''),
  );
  async function load(id: string) {
    const current = ++generation;
    try {
      const result = await api.fix(id);
      if (current !== generation) return;
      if (detail?.fix.id !== id) {
        delivery = 'patch';
        once = false;
        note = '';
      }
      detail = result;
      if (result.actions.confirm) executionEnabled = (await api.fixSettings()).fixes.execution;
      error = '';
    } catch (cause) {
      if (current === generation) error = errorMessage(cause);
    }
  }
  $effect(() => {
    const id = page.params.id;
    if (id) untrack(() => void load(id));
  });
  $effect(() => {
    if ($live.fix?.id === page.params.id) untrack(() => void load(page.params.id!));
  });
  async function action(which: FixAction) {
    if (!detail) return;
    if (
      which === 'approve' &&
      !window.confirm(
        `Publish this patch to ${delivery === 'same_branch' ? 'the original PR branch' : 'a new draft pull request'}? Revera will recheck the PR head and write permissions first.`,
      )
    )
      return;
    if (
      which === 'discard' &&
      !window.confirm('Discard this preview? It will no longer be publishable.')
    )
      return;
    busy = which;
    error = '';
    message = '';
    try {
      await api.fixAction(
        detail.fix.id,
        which,
        which === 'approve'
          ? { delivery }
          : which === 'confirm'
            ? { once: !executionEnabled && once }
            : which === 'regenerate'
              ? { note }
              : undefined,
      );
      await load(detail.fix.id);
      message =
        which === 'approve'
          ? 'Publication requested. Follow the status below.'
          : which === 'regenerate'
            ? 'Regeneration queued within this request’s attempt budget.'
            : which === 'confirm'
              ? 'Preview generation confirmed.'
              : which === 'discard'
                ? 'Preview discarded.'
                : 'Request cancelled.';
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = '';
    }
  }
  async function artifact(which: 'patch.diff' | 'instructions.md', copy = false) {
    if (!detail) return;
    busy = which;
    error = '';
    try {
      const text = await api.fixArtifact(detail.fix.id, which);
      if (copy) {
        await navigator.clipboard.writeText(text);
        message = 'Agent instructions copied.';
      } else {
        const url = URL.createObjectURL(
          new Blob([text], { type: which === 'patch.diff' ? 'text/x-diff' : 'text/markdown' }),
        );
        const link = document.createElement('a');
        link.href = url;
        link.download = `revera-${detail.fix.id}-${which}`;
        link.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      }
    } catch (cause) {
      error = copy
        ? 'Could not copy instructions. Download the instructions instead.'
        : errorMessage(cause);
    } finally {
      busy = '';
    }
  }
</script>

<svelte:head><title>Fix preview · Revera</title></svelte:head>
<a class="back-link" href="/fixes">← All fixes</a>
{#if error}<Notice message={error} retry={() => load(page.params.id!)} />{/if}
{#if message}<Notice {message} kind="success" />{/if}
{#if detail}
  {@const fix = detail.fix}
  <div class="detail-header">
    <FixStatus state={fix.state} />
    <h1>{fix.pr_title}</h1>
    <div class="row-meta">
      <span>{fix.repo}</span><a class="text-link" href={fix.pr_url} target="_blank" rel="noreferrer"
        >PR #{fix.pr}</a
      ><span>@</span><a
        class="mono text-link"
        href={`https://github.com/${fix.repo}/commit/${fix.head_sha}`}
        target="_blank"
        rel="noreferrer"
        title={fix.head_sha}>{fix.head_sha.slice(0, 12)}</a
      >{#if fix.repo_source === 'external'}<span class="badge">External · private preview</span
        >{/if}
    </div>
  </div>
  {#if running}<div class="notice" role="status">
      <span>{fix.progress || 'Waiting for the next stage…'}</span><span
        class="live-label"
        class:offline={!$live.connected}>{$live.connected ? 'Live' : 'Reconnecting'}</span
      >
    </div>{/if}
  {#if fix.reason}<div class="notice" role="note">{fix.reason}</div>{/if}
  <div class="detail-layout">
    <div class="fix-content">
      {#if fix.generation}<p class="detail-summary">{fix.generation.summary}</p>{/if}
      <div class="section-heading">
        <h2>Findings <span class="helper">{fix.findings.length}</span></h2>
        <span class="helper">Fixer claims and independent verdicts</span>
      </div>
      {#each fix.findings as finding}
        {@const generated = fix.generation?.per_finding.find((f) => f.id === finding.id)}
        {@const verified = fix.verification?.per_finding.find((f) => f.id === finding.id)}
        <article class="finding">
          <div class="finding-top">
            <Status status={finding.severity} /><code>{finding.id}</code>
          </div>
          <h3 class="finding-title">{finding.title}</h3>
          <p class="helper mono">
            {finding.file}:{finding.start_line}{finding.end_line > finding.start_line
              ? `–${finding.end_line}`
              : ''}
          </p>
          <p class="finding-body">
            <strong>Fixer:</strong>
            {generated
              ? `${generated.disposition.replaceAll('_', ' ')} — ${generated.note}`
              : 'No disposition yet.'}
          </p>
          <p class="finding-body">
            <strong>Independent verifier:</strong>
            {verified
              ? `${verified.verdict} — ${verified.rationale}`
              : 'Not verified yet. A fixer claim is not proof of resolution.'}
          </p>
        </article>
      {/each}
      {#if fix.tests}<section class="section">
          <div class="section-heading">
            <h2>Sandboxed tests</h2>
            <Status
              status={fix.tests.outcome === 'passed'
                ? 'complete'
                : fix.tests.outcome === 'not_configured'
                  ? 'partial'
                  : 'failed'}
              label={fix.tests.outcome.replaceAll('_', ' ')}
            />
          </div>
          {#if fix.tests.outcome === 'not_configured'}<p class="helper">
              No test command was configured. This patch has not passed repository tests.
            </p>{/if}{#if fix.tests.output}<details class="data-disclosure">
              <summary>Test output</summary>
              <pre>{fix.tests.output}</pre>
            </details>{/if}
        </section>{/if}
      {#if fix.patch?.warnings.length || fix.patch?.errors.length || fix.verification?.new_problems.length}<section
          class="section"
        >
          <h2>Warnings and errors</h2>
          {#each fix.patch?.warnings || [] as warning}<p class="notice fix-notice" role="note">
              {warning}
            </p>{/each}{#each [...(fix.patch?.errors || []), ...(fix.verification?.new_problems || [])] as problem}<Notice
              message={problem}
            />{/each}
        </section>{/if}
      {#if detail.patch && fix.patch}<FixDiff
          patch={detail.patch}
          files={fix.patch.files}
        />{:else if !running && !detail.actions.confirm}<p class="helper section">
          No patch was produced.
        </p>{/if}
      {#if fix.delivery}<section class="section">
          <div class="section-heading">
            <h2>Published change</h2>
            <a class="button small" href={fix.delivery.url} target="_blank" rel="noreferrer"
              >Open {fix.delivery.mode === 'draft_pr' ? 'draft pull request' : 'published commit'} ↗</a
            >
          </div>
          <p class="helper mono">{fix.delivery.branch} · {fix.delivery.commit.slice(0, 12)}</p>
          {#if fix.delivery.error}<Notice message={fix.delivery.error} />{/if}
        </section>{/if}
      {#if fix.verify_review}<section class="section">
          <div class="section-heading">
            <h2>Follow-up review</h2>
            <a class="text-link helper" href={`/reviews/${fix.verify_review.job}`}>Open review</a>
          </div>
          <p class="helper">
            {fix.verify_review.state.replaceAll('_', ' ')} · {fix.verify_review.resolved.length} resolved
            · {fix.verify_review.still_open.length} still open · {fix.verify_review.new_findings} new
            findings
          </p>
          {#if fix.verify_review.resolved.length}<p class="finding-body">
              Resolved: <code>{fix.verify_review.resolved.join(', ')}</code>
            </p>{/if}{#if fix.verify_review.still_open.length}<p class="finding-body">
              Still open: <code>{fix.verify_review.still_open.join(', ')}</code>
            </p>{/if}
          <p class="finding-body">{fix.verify_review.note}</p>
        </section>{/if}
      <details class="data-disclosure">
        <summary>Audit log ({fix.events.length})</summary>
        <ol class="audit-log">
          {#each fix.events as event}<li>
              <time datetime={event.at}>{new Date(event.at).toLocaleString()}</time> ·
              <strong>{event.actor}</strong>
              · {event.action.replaceAll('_', ' ')}
              <p>{event.detail}</p>
            </li>{/each}
        </ol>
      </details>
    </div>
    <aside class="detail-meta" aria-label="Fix request details">
      <h3>Request details</h3>
      <dl>
        <div>
          <dt>Requested by</dt>
          <dd>{fix.requested_by}</dd>
        </div>
        <div>
          <dt>Source</dt>
          <dd>{fix.source}</dd>
        </div>
        <div>
          <dt>Consent</dt>
          <dd>
            {fix.consent === 'once'
              ? 'This run only'
              : fix.consent === 'persistent'
                ? 'Persistent execution'
                : 'Not granted'}
          </dd>
        </div>
        <div>
          <dt>Attempt</dt>
          <dd>{fix.attempt} / {fix.max_attempts}</dd>
        </div>
        <div>
          <dt>Review</dt>
          <dd><a class="text-link" href={`/reviews/${fix.review_job}`}>{fix.review_job}</a></dd>
        </div>
      </dl>
      {#if fix.usage}<h3>Model usage</h3>
        <dl>
          <div>
            <dt>Requests</dt>
            <dd>{fix.usage.requests}</dd>
          </div>
          <div>
            <dt>Prompt tokens</dt>
            <dd>{fix.usage.prompt_tokens.toLocaleString()}</dd>
          </div>
          <div>
            <dt>Completion tokens</dt>
            <dd>{fix.usage.completion_tokens.toLocaleString()}</dd>
          </div>
          <div>
            <dt>Routes</dt>
            <dd>{fix.usage.routes.join(', ')}</dd>
          </div>
        </dl>
        <p class="helper">No cost estimate available for these routes.</p>{/if}
      {#if detail.actions.confirm}<section class="section">
          <h3>Confirm generation</h3>
          <p class="helper">This starts sandboxed model work, not a GitHub write.</p>
          {#if !fix.consent && !executionEnabled}<label class="once-consent"
              ><input type="checkbox" bind:checked={once} /> Allow generation for this run only</label
            >{/if}<button
            class="button primary"
            disabled={!!busy || (!fix.consent && !executionEnabled && !once)}
            onclick={() => action('confirm')}>Confirm preview</button
          >{#if !fix.consent && !executionEnabled && !once}<p class="helper">
              Grant one-time consent, or <a class="text-link" href="/settings?tab=fixes"
                >enable execution in Settings</a
              >.
            </p>{/if}
        </section>{/if}
      {#if fix.state === 'ready'}<FixDelivery
          capabilities={detail.delivery}
          bind:value={delivery}
          selectable
        /><button
          class="button primary"
          disabled={!!busy || !canPublish}
          aria-describedby="approve-reason"
          onclick={() => action('approve')}>Approve &amp; publish</button
        >
        <p class="helper" id="approve-reason">
          {canPublish
            ? 'Approves this patch and writes only to the selected destination. The server rechecks the exact PR head.'
            : publishReason}
        </p>{/if}
      <div class="actions section">
        {#if detail.patch}<button
            class="button"
            disabled={!!busy}
            onclick={() => artifact('patch.diff')}>Download patch</button
          >{/if}
        <button class="button" disabled={!!busy} onclick={() => artifact('instructions.md', true)}
          >Copy agent instructions</button
        ><button class="button" disabled={!!busy} onclick={() => artifact('instructions.md')}
          >Download instructions</button
        >
        {#if detail.actions.cancel}<button
            class="button danger"
            disabled={!!busy}
            onclick={() => action('cancel')}>Cancel request</button
          >{/if}
        {#if detail.actions.discard}<button
            class="button danger"
            disabled={!!busy}
            onclick={() => action('discard')}>Discard preview</button
          >{/if}
      </div>
      {#if ['ready', 'validation_failed', 'failed', 'cancelled'].includes(fix.state)}<details
          class="data-disclosure"
        >
          <summary>Regenerate preview</summary>
          <div class="field section">
            <label for="fix-guidance">Optional guidance</label><textarea
              id="fix-guidance"
              maxlength="2000"
              rows="3"
              bind:value={note}></textarea><small
              >Up to 2,000 characters. Uses another attempt, not a new budget.</small
            ><button
              class="button"
              disabled={!!busy || !detail.actions.regenerate}
              aria-describedby="regenerate-reason"
              onclick={() => action('regenerate')}>Regenerate</button
            >
            <p class="helper" id="regenerate-reason">
              {detail.reasons.regenerate || `${fix.max_attempts - fix.attempt} attempts remaining.`}
            </p>
          </div>
        </details>{/if}
    </aside>
  </div>
{:else if !error}<p class="loading" role="status">Loading fix preview…</p>{/if}

<style>
  .fix-content {
    min-width: 0;
  }
  .fix-notice {
    margin-top: 12px;
  }
  .audit-log {
    padding-left: 20px;
    line-height: 1.8;
  }
  .audit-log li {
    padding: 8px 0;
  }
  .once-consent {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    font-size: 12px;
    margin: 16px 0;
  }
  .once-consent input {
    flex-shrink: 0;
  }
  .detail-meta .helper {
    margin: 8px 0;
  }
  .detail-meta .section h3 {
    margin-bottom: 12px;
  }
  .finding-body,
  .audit-log {
    overflow-wrap: anywhere;
  }
</style>
