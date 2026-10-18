<script lang="ts">
  import { onMount } from 'svelte';
  import { api, errorMessage } from '#lib/api';
  import { modes } from '#lib/config.svelte';
  import type { ExternalPR, TrackedPR, ReviewMode } from '#lib/types';
  import SelectField from './SelectField.svelte';
  import Notice from './Notice.svelte';
  let { onqueued }: { onqueued: (id: string) => void } = $props();
  let enabled = $state(false);
  let tracked = $state<TrackedPR[]>([]);
  let prs = $state<ExternalPR[] | null>(null);
  let incomplete = $state(false);
  let mode = $state<ReviewMode>('standard');
  let error = $state('');
  let busy = $state(false);
  let message = $state('');
  async function load() {
    const [policy, result] = await Promise.all([api.externalPolicy(), api.externalTracking()]);
    enabled = policy.tracking_enabled;
    tracked = result.tracked;
  }
  onMount(() => {
    void load().catch((cause) => (error = errorMessage(cause)));
  });
  async function find() {
    busy = true;
    try {
      await load();
      const result = await api.externalDiscover();
      prs = result.prs;
      incomplete = result.incomplete;
      error = '';
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
  async function act(pr: ExternalPR, action: 'track' | 'review' | 'ignore') {
    busy = true;
    try {
      if (action === 'track') {
        await api.externalTrack(pr.url, mode);
        message = `Tracking ${pr.repo} #${pr.number}. Its initial review is queued on the next check.`;
        await load();
      }
      if (action === 'review') {
        const job = await api.externalReview(pr.url, mode);
        onqueued(job.id);
        message = 'Private review queued.';
      }
      if (action === 'ignore') {
        await api.externalIgnore(pr.url);
        message = 'PR ignored in discovery.';
      }
      prs = prs?.filter((item) => item.url !== pr.url) ?? null;
      error = '';
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
  async function untrack(pr: TrackedPR) {
    busy = true;
    try {
      await api.externalUntrack(pr.repo_id, pr.number);
      await load();
      error = '';
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
</script>

<details class="card" style="margin-bottom: 20px">
  <summary>Track my external PRs</summary>
  <p class="muted">
    Find public open PRs authored by your signed-in GitHub account, excluding installed
    repositories. Choose Track, Review once, or Ignore. Nothing is automatically selected.
  </p>
  <p class="muted">
    Tracking is {enabled ? 'enabled' : 'off'}.
    <a href="/settings#external-prs">External PR settings</a>
  </p>
  <button class="button" disabled={busy} onclick={find}
    >{busy ? 'Working…' : 'Find my open PRs'}</button
  >
  {#if prs}
    <div class="field" style="margin-top: 16px">
      <label for="tracking-mode">Tracking review mode</label><SelectField
        id="tracking-mode"
        value={mode}
        options={modes}
        onchange={(value) => (mode = value as ReviewMode)}
      />
    </div>
    {#if incomplete}<p class="muted">
        Showing the first 100 results; GitHub search may be incomplete. Paste a PR URL to review
        other results.
      </p>{/if}
    {#each prs as pr (pr.url)}
      <div class="tracking-row">
        <div>
          <a href={pr.url} target="_blank" rel="noreferrer">{pr.title}</a>
          <div class="muted">{pr.repo} #{pr.number}</div>
        </div>
        <div class="actions" aria-label={`Choose action for ${pr.repo} #${pr.number}`}>
          <button
            class="button"
            disabled={busy ||
              !enabled ||
              tracked.some((item) => item.url === pr.url && item.status === 'tracking')}
            title={!enabled ? 'Enable tracking in External PR settings first' : ''}
            onclick={() => act(pr, 'track')}>Track</button
          >
          <button class="button" disabled={busy} onclick={() => act(pr, 'review')}
            >Review once</button
          >
          <button class="button" disabled={busy} onclick={() => act(pr, 'ignore')}>Ignore</button>
        </div>
      </div>
    {:else}<p class="muted">No unselected public open PRs found.</p>{/each}
  {/if}
  {#each tracked as pr (pr.url)}
    <div class="tracking-row">
      <div>
        <a href={pr.url} target="_blank" rel="noreferrer">{pr.repo} #{pr.number}</a>
        <div class="muted">
          {pr.status}{pr.reason ? ` · ${pr.reason}` : ''}{pr.head_sha
            ? ` · ${pr.head_sha.slice(0, 7)}`
            : ''}
        </div>
      </div>
      {#if pr.status === 'tracking'}<button
          class="button"
          disabled={busy}
          onclick={() => untrack(pr)}>Untrack</button
        >{/if}
    </div>
  {/each}
  {#if message}<p role="status">{message}</p>{/if}
  {#if error}<Notice message={error} />{/if}
</details>

<style>
  .tracking-row {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
    padding: 12px 0;
    border-bottom: 1px solid var(--border);
  }
</style>
