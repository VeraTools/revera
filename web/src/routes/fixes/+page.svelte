<script lang="ts">
  import { untrack } from 'svelte';
  import { page } from '$app/state';
  import { replaceState } from '$app/navigation';
  import { api, errorMessage } from '#lib/api';
  import { live } from '#lib/events';
  import { relative } from '#lib/time';
  import type { FixSummary } from '#lib/types';
  import FixStatus from '#lib/components/FixStatus.svelte';
  import SelectField from '#lib/components/SelectField.svelte';
  import Notice from '#lib/components/Notice.svelte';
  let fixes = $state<FixSummary[]>([]);
  let stateFilter = $state('');
  let error = $state('');
  let loading = $state(true);
  const states = [
    'awaiting_confirmation',
    'queued',
    'generating',
    'validating',
    'ready',
    'publishing',
    'published',
    'verifying',
    'verified',
    'failed',
    'validation_failed',
    'publish_failed',
    'cancelled',
    'discarded',
    'stale',
    'unsupported',
  ];
  const filtered = $derived(fixes.filter((f) => !stateFilter || f.state === stateFilter));
  const liveFix = $derived($live.fix);
  let generation = 0;
  async function load() {
    const current = ++generation;
    try {
      const result = (await api.fixes()).fixes;
      if (current !== generation) return;
      fixes = result;
      error = '';
    } catch (cause) {
      if (current === generation) error = errorMessage(cause);
    } finally {
      if (current === generation) loading = false;
    }
  }
  $effect(() => {
    const query = page.url.searchParams.get('state') || '';
    untrack(() => {
      stateFilter = states.includes(query) ? query : '';
    });
  });
  $effect(() => {
    liveFix;
    untrack(() => void load());
  });
  function filter(value: string) {
    stateFilter = value;
    const url = new URL(page.url.href);
    if (value) url.searchParams.set('state', value);
    else url.searchParams.delete('state');
    replaceState(url, {});
  }
</script>

<svelte:head><title>Fixes · Revera</title></svelte:head>
<div class="page-header">
  <div>
    <h1>Fixes</h1>
    <p>Private previews and approved changes, tied to the reviewed commit.</p>
  </div>
  <a class="button" href="/settings?tab=fixes">Fix settings</a>
</div>
<div class="filters">
  <SelectField
    id="fix-state"
    label="Fix state"
    value={stateFilter}
    onchange={filter}
    options={[
      { value: '', label: 'All states' },
      ...states.map((value) => ({ value, label: value.replaceAll('_', ' ') })),
    ]}
  /><span class="result-count"
    >{filtered.length} {filtered.length === 1 ? 'request' : 'requests'}</span
  >
</div>
{#if error}<Notice message={error} retry={load} />{/if}
{#if filtered.length}<div class="table-scroll">
    <table class="fixes-table">
      <thead
        ><tr
          ><th>Pull request / reviewed SHA</th><th>Requested by</th><th>Source</th><th>State</th><th
            >Findings</th
          ><th>Updated</th></tr
        ></thead
      ><tbody>
        {#each filtered as fix}<tr
            ><td
              ><a class="row-title" href={`/fixes/${fix.id}`}>{fix.pr_title}</a>
              <div class="row-meta">
                {fix.repo}
                <a class="text-link" href={fix.pr_url} target="_blank" rel="noreferrer">#{fix.pr}</a
                >
                @
                <code title={fix.head_sha}>{fix.head_sha.slice(0, 8)}</code
                >{#if fix.repo_source === 'external'}<span class="badge">External · patch only</span
                  >{/if}
              </div></td
            ><td>{fix.requested_by}</td><td>{fix.source}</td><td><FixStatus state={fix.state} /></td
            ><td class="tabular">{fix.findings_count}</td><td class="muted nowrap"
              ><time datetime={fix.updated_at} title={new Date(fix.updated_at).toLocaleString()}
                >{relative(fix.updated_at)}</time
              ></td
            ></tr
          >{/each}
      </tbody>
    </table>
  </div>{:else if loading}<p class="loading" role="status">Loading fixes…</p>{:else if !error}<div
    class="empty-state"
  >
    <p>
      {stateFilter
        ? 'No fix requests in this state.'
        : 'No fix requests yet. Open a complete review with accepted findings and choose “Fix with Revera”, or request a fix with a PR mention or the GitHub Checks action. Generating a preview needs your consent.'}
    </p>
    <a class="button" href="/reviews">Open reviews</a>
  </div>{/if}

<style>
  .fixes-table {
    min-width: 800px;
  }
  .fixes-table th:first-child {
    width: 44%;
  }
</style>
