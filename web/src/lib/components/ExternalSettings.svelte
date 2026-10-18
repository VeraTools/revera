<script lang="ts">
  import { onMount } from 'svelte';
  import { api, errorMessage } from '#lib/api';
  import type { ExternalPolicy } from '#lib/types';
  import Notice from './Notice.svelte';
  let policy = $state<ExternalPolicy>();
  let error = $state('');
  let saved = $state(false);
  let busy = $state(false);
  onMount(() => {
    void api
      .externalPolicy()
      .then((value) => (policy = value))
      .catch((cause) => (error = errorMessage(cause)));
  });
  async function save() {
    if (!policy) return;
    busy = true;
    saved = false;
    try {
      await api.saveExternalPolicy(policy);
      error = '';
      saved = true;
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
</script>

<section id="external-prs" class="card" style="margin-top: 24px" aria-label="External PR settings">
  <h2>External PRs</h2>
  <p class="muted">
    Public repositories only. Reviews and downloadable fixes stay private; an external review never
    posts or pushes upstream. Reviews use the operator's models and budgets, not repository
    configuration or guidance.
  </p>
  {#if policy}
    <label
      ><input
        type="checkbox"
        bind:checked={policy.tracking_enabled}
        onchange={() => (saved = false)}
      /> Track my external PRs</label
    >
    <p class="muted">
      Off by default. In Reviews, find your open PRs and explicitly select which to track. Disabling
      pauses polling without changing your selections. Closing or untracking a PR stops it.
    </p>
    <div class="field">
      <label for="external-poll-minutes">Check tracked PRs every (minutes)</label>
      <input
        id="external-poll-minutes"
        type="number"
        min="15"
        max="1440"
        bind:value={policy.poll_minutes}
        oninput={() => (saved = false)}
      />
      <small
        >15–1440 minutes; default 60. Runs on the server's reconciliation cadence. Only new heads
        start reviews.</small
      >
    </div>
    <p class="muted">
      Up to {policy.max_tracked} tracked PRs, {policy.reviews_per_hour} reviews/hour and {policy.concurrency}
      external review at a time. Repository limit: {Math.round(policy.max_repo_size_kb / 1024)} MB. Checkouts
      expire after {policy.retention_days} days (at most {policy.max_checkouts} retained).
    </p>
    <button
      class="button"
      disabled={busy ||
        !policy.poll_minutes ||
        policy.poll_minutes < 15 ||
        policy.poll_minutes > 1440}
      onclick={save}>Save external settings</button
    >
  {/if}
  {#if saved}<p role="status">External settings saved.</p>{/if}
  {#if error}<Notice message={error} />{/if}
</section>
