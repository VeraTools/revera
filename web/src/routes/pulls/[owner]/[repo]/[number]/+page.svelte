<script lang="ts">
  import { untrack } from 'svelte';
  import { page } from '$app/state';
  import { ArrowLeft, ArrowRight, ArrowUpRight } from '@lucide/svelte';
  import { api, ApiError, errorMessage } from '#lib/api';
  import { live } from '#lib/events';
  import { createNotices } from '#lib/notices.svelte';
  import { labelStyle, reviewDecision, reviewing } from '../../../../../lib/pulls';
  import type { PullDetail, JobSummary, FixSummary } from '#lib/types';
  import Notice from '#lib/components/Notice.svelte';
  import PullActions from '#lib/components/PullActions.svelte';
  import PullRevera from '#lib/components/PullRevera.svelte';
  import PullConversation from '#lib/components/PullConversation.svelte';
  import GithubBody from '#lib/components/GithubBody.svelte';
  const notice = createNotices();
  let detail = $state<PullDetail>();
  let loading = $state(true),
    error = $state(''),
    notFound = $state(false);
  let generation = 0;
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  let seenJob: JobSummary | undefined,
    seenFix: FixSummary | undefined,
    seenRepos = 0;
  const route = $derived({
    owner: page.params.owner!,
    repo: page.params.repo!,
    number: Number(page.params.number),
  });
  async function load() {
    const current = ++generation;
    const target = { ...route };
    try {
      const result = await api.pull(target.owner, target.repo, target.number);
      if (current === generation) {
        detail = result;
        error = '';
        notFound = false;
      }
    } catch (cause) {
      if (current !== generation) return;
      notFound = cause instanceof ApiError && cause.status === 404;
      error =
        cause instanceof ApiError && cause.status === 502
          ? 'GitHub is unavailable right now'
          : errorMessage(cause);
    } finally {
      if (current === generation) loading = false;
    }
  }
  $effect(() => {
    route;
    untrack(() => {
      detail = undefined;
      loading = true;
      error = '';
      notFound = false;
      notice.state.error = '';
      notice.state.message = '';
      void load();
    });
    return () => {
      clearTimeout(refreshTimer);
      refreshTimer = undefined;
      generation++;
    };
  });
  $effect(() => {
    const { job, fix, repos } = $live;
    untrack(() => {
      // Fix events use the canonical repository name, not its numeric ID.
      const changed =
        repos !== seenRepos ||
        (job !== seenJob &&
          job &&
          detail &&
          job.repo_id === detail.repo_id &&
          job.pr === detail.number) ||
        (fix !== seenFix &&
          fix &&
          detail &&
          fix.repo_source === 'installed' &&
          fix.repo === detail.repo &&
          fix.pr === detail.number);
      seenJob = job;
      seenFix = fix;
      seenRepos = repos;
      if (!changed || !detail || refreshTimer !== undefined) return;
      // Event churn must not cancel a pending refresh. Route cleanup owns this timer.
      refreshTimer = setTimeout(() => {
        refreshTimer = undefined;
        void load();
      }, 500);
    });
  });
  function queued(id: string) {
    if (detail) detail = { ...detail, review: reviewing(detail.review, id) };
  }
</script>

<svelte:head
  ><title>{detail ? `${detail.title} · Revera` : 'Pull request · Revera'}</title></svelte:head
>
<div class="pull-breadcrumb">
  <a class="back-link" href="/pulls"><ArrowLeft size={14} />Pull requests</a><span class="muted"
    >/</span
  ><span>{route.owner}/{route.repo} #{route.number}</span>
</div>
{#if notFound}<div class="empty-state">
    <h1>Pull request not found</h1>
    <p>This repository is not installed or is no longer available.</p>
    <a class="button" href="/pulls">Back to pull requests</a>
  </div>
{:else}
  {#if error}<Notice message={error} retry={load} />{/if}
  {#if notice.state.error}<Notice message={notice.state.error} />{/if}
  {#if notice.state.message}<Notice message={notice.state.message} kind="success" />{/if}
  {#if detail}
    <header class="detail-header pull-header">
      <div class="header-copy">
        <div class="actions">
          <span class="muted">#{detail.number}</span>{#if detail.draft}<span class="badge"
              >Draft</span
            >{/if}{#if detail.fork}<span class="badge">Fork</span>{/if}{#if detail.merged}<span
              class="badge">Merged</span
            >{:else if detail.state !== 'open'}<span class="badge">Closed</span>{/if}
        </div>
        <h1>{detail.title}</h1>
        <div class="row-meta">
          <span>{detail.author.login || 'Deleted user'}</span><span>·</span><code
            >{detail.head_ref}</code
          ><ArrowRight size={12} /><code>{detail.base_ref}</code><span>·</span><span
            class="additions">+{detail.additions}</span
          ><span class="deletions">−{detail.deletions}</span><span
            >{detail.changed_files} {detail.changed_files === 1 ? 'file' : 'files'}</span
          >{#if detail.review_decision}<span>·</span><span
              >{reviewDecision(detail.review_decision)}</span
            >{/if}
        </div>
        {#if detail.labels.length}<div class="labels">
            {#each detail.labels as label}<span class="badge" style={labelStyle(label.color)}
                >{label.name}</span
              >{/each}
          </div>{/if}
      </div>
      <div class="header-actions">
        <PullActions
          pull={detail}
          closed={detail.state !== 'open'}
          onqueued={queued}
          onrefresh={load}
          onsuccess={notice.success}
          onerror={notice.failure}
        /><a class="button small ghost" href={detail.url} target="_blank" rel="noreferrer"
          >Open on GitHub<ArrowUpRight size={13} /></a
        >
      </div>
    </header>
    <div class="pull-detail-layout">
      <div class="conversation-main">
        <section class="description-card" aria-labelledby="description-heading">
          <h2 id="description-heading">Description</h2>
          {#if detail.body}<GithubBody body={detail.body} collapsible />{:else}<p
              class="muted no-description"
            >
              No description
            </p>{/if}
        </section>
        <PullConversation {detail} />
      </div>
      <PullRevera {detail} />
    </div>
  {:else if loading}<div class="pull-skeleton" role="status" aria-label="Loading pull request">
      <span class="sr-only">Loading pull request…</span>
      <div></div>
      <div></div>
      <div></div>
    </div>{/if}
{/if}

<style>
  .pull-breadcrumb {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
    font-size: 12px;
    margin-bottom: 24px;
    overflow-wrap: anywhere;
  }
  .pull-breadcrumb .back-link {
    margin: 0;
  }
  .pull-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 24px;
  }
  .header-copy {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .header-actions {
    display: grid;
    justify-items: end;
    gap: 8px;
    flex-shrink: 0;
  }
  .additions {
    color: var(--green);
  }
  .deletions {
    color: var(--red);
  }
  .labels {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 14px;
  }
  .pull-detail-layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 264px;
    align-items: start;
    gap: 28px;
  }
  .conversation-main {
    min-width: 0;
  }
  .description-card {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 20px;
  }
  .description-card h2 {
    font-size: 14px;
    margin-bottom: 16px;
  }
  .no-description {
    font-size: 13px;
  }
  .pull-skeleton {
    display: grid;
    gap: 24px;
  }
  .pull-skeleton div {
    border-radius: 6px;
    background: var(--surface);
    border: 1px solid var(--border);
    height: 100px;
  }
  .pull-skeleton div:nth-child(3) {
    height: 240px;
  }
  @media (max-width: 1000px) {
    .pull-detail-layout {
      grid-template-columns: minmax(0, 1fr);
    }
    .conversation-main {
      grid-row: 2;
    }
  }
  @media (max-width: 760px) {
    .pull-header {
      flex-direction: column;
      gap: 16px;
    }
    .header-actions {
      display: flex;
      justify-content: space-between;
      align-items: center;
      width: 100%;
    }
    .pull-breadcrumb {
      font-size: 11px;
    }
    .description-card {
      padding: 16px;
    }
  }
</style>
