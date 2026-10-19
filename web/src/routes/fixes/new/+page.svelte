<script lang="ts">
  import { untrack } from 'svelte';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { api, errorMessage } from '#lib/api';
  import type { FixPlan } from '#lib/types';
  import FixDelivery from '#lib/components/FixDelivery.svelte';
  import Notice from '#lib/components/Notice.svelte';
  import Status from '#lib/components/Status.svelte';
  let plan = $state<FixPlan>();
  let selected = $state<string[]>([]);
  let existing = $state<FixPlan['existing']>(null);
  let error = $state('');
  let busy = $state(false);
  let selectionBusy = $state(false);
  let revision = 0;
  async function load(job: string, ids?: string[]) {
    try {
      plan = await api.fixPlan(job, ids);
      selected = plan.findings.map((f) => f.id);
      existing = plan.existing;
      error = '';
    } catch (cause) {
      error = errorMessage(cause);
    }
  }
  $effect(() => {
    const job = page.url.searchParams.get('job');
    const ids = page.url.searchParams.get('findings');
    untrack(() => {
      if (job) void load(job, ids ? ids.split(',') : undefined);
      else error = 'Open Fix with Revera from a complete review.';
    });
  });
  async function select(id: string, checked: boolean) {
    selected = checked ? [...selected, id] : selected.filter((item) => item !== id);
    const current = ++revision;
    selectionBusy = true;
    try {
      if (!selected.length) {
        existing = null;
        error = '';
        return;
      }
      const result = await api.fixPlan(plan!.job.id, [...selected]);
      if (current === revision) {
        existing = result.existing;
        error = '';
      }
    } catch (cause) {
      if (current === revision) error = errorMessage(cause);
    } finally {
      if (current === revision) selectionBusy = false;
    }
  }
  async function generate() {
    if (!plan || !selected.length) return;
    busy = true;
    error = '';
    try {
      const { fix } = await api.requestFix(plan.job.id, selected, plan.consent_required);
      await goto(`/fixes/${fix.id}`);
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>New fix preview · Revera</title></svelte:head>
<a class="back-link" href={plan ? `/reviews/${plan.job.id}` : '/reviews'}>← Back to review</a>
<div class="page-header">
  <div>
    <h1>Generate a fix preview</h1>
    <p>Review the scope before any model work begins.</p>
  </div>
</div>
{#if error}<Notice message={error} />{/if}
{#if plan}<div class="detail-layout">
    <div>
      <div class="notice" role="note">
        Revera prepares a minimal patch in an isolated sandbox, checks it, and asks an independent
        verifier to inspect the result. Generation does not write to GitHub. Publishing from this
        flow requires your approval.
      </div>
      <div class="section-heading">
        <div>
          <h2>{plan.job.title}</h2>
          <p>
            {plan.job.repo} ·
            <a class="text-link" href={plan.job.pr_url} target="_blank" rel="noreferrer"
              >#{plan.job.pr}</a
            >
            @ <code title={plan.job.head_sha}>{plan.job.head_sha?.slice(0, 12)}</code>
          </p>
        </div>
      </div>
      <div class="section-heading">
        <h2>Findings to fix</h2>
        <span class="helper">{selected.length} selected</span>
      </div>
      {#each plan.findings as finding}<label class="plan-finding"
          ><input
            type="checkbox"
            checked={selected.includes(finding.id)}
            onchange={(event) => void select(finding.id, event.currentTarget.checked)}
          /><span
            ><span class="finding-top"
              ><Status status={finding.severity} /><code>{finding.id}</code></span
            ><strong>{finding.title}</strong><span class="helper mono"
              >{finding.file}:{finding.start_line}</span
            ></span
          ></label
        >{/each}
      <FixDelivery capabilities={plan.delivery} />
      {#if plan.blocked}<Notice message={plan.blocked} />{:else if existing}<div
          class="notice"
          role="note"
        >
          <span>A request already exists for these findings at this SHA.</span><a
            class="button small"
            href={`/fixes/${existing.id}`}>Open existing request</a
          >
        </div>{:else}<div class="section actions">
          <button
            class="button primary"
            disabled={busy || selectionBusy || !selected.length || !plan.sandbox.available}
            onclick={generate}
            >{busy
              ? 'Requesting…'
              : plan.execution_enabled
                ? 'Generate preview'
                : 'Generate preview (this run only)'}</button
          >{#if !plan.execution_enabled}<a class="text-link helper" href="/settings?tab=fixes"
              >Enable fixes in Settings</a
            >{/if}
        </div>
        {#if !selected.length}<p class="helper">
            Select at least one finding to fix.
          </p>{/if}{#if !plan.execution_enabled}<p class="helper consent-note">
            One-time consent applies only to this request. Persistent execution stays off.
          </p>{/if}{/if}
    </div>
    <aside class="detail-meta">
      <h3>Run limits</h3>
      <dl>
        <div>
          <dt>Sandbox</dt>
          <dd>
            <Status
              status={plan.sandbox.available ? 'ready' : 'failed'}
              label={plan.sandbox.available ? 'Available' : 'Unavailable'}
            />{#if plan.sandbox.reason}<p class="helper">
                {plan.sandbox.reason}
              </p>{/if}{#if plan.sandbox.available && !plan.sandbox.proc}<p class="helper">
                Runs without a /proc mount.
              </p>{/if}
          </dd>
        </div>
        <div>
          <dt>Maximum model requests</dt>
          <dd>{plan.estimate.max_model_requests}</dd>
        </div>
        <div>
          <dt>Maximum runtime</dt>
          <dd>{plan.estimate.max_seconds} seconds</dd>
        </div>
        <div>
          <dt>Attempts</dt>
          <dd>{plan.estimate.attempts}</dd>
        </div>
        <div>
          <dt>Patch cap</dt>
          <dd>{plan.limits.max_files} files · {plan.limits.max_patch_kb} KB</dd>
        </div>
        <div>
          <dt>Tool calls</dt>
          <dd>{plan.limits.tool_calls}</dd>
        </div>
      </dl>
      <p class="helper">
        These are ceilings, not a price quote. Usage depends on the configured routes; no cost
        estimate is available.
      </p>
    </aside>
  </div>{:else if !error}<p class="loading" role="status">Loading fix plan…</p>{/if}

<style>
  .plan-finding {
    display: flex;
    gap: 12px;
    padding: 18px 0;
    border-bottom: 1px solid var(--border);
    cursor: pointer;
  }
  .plan-finding > span {
    display: grid;
    gap: 8px;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .plan-finding input {
    flex-shrink: 0;
    margin-top: 4px;
  }
  strong {
    font-weight: 500;
    font-size: 13px;
  }
  .consent-note {
    margin-top: 12px;
  }
</style>
