<script lang="ts">
  import { onMount } from 'svelte';
  import { Dialog, DropdownMenu } from 'bits-ui';
  import {
    Search,
    MoreHorizontal,
    ArrowUpRight,
    LockKeyhole,
    GitPullRequest,
    RefreshCw,
  } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import { live } from '#lib/events';
  import { relative } from '#lib/time';
  import type { Repository, PullRequest } from '#lib/types';
  import Toggle from '#lib/components/Toggle.svelte';
  import Status from '#lib/components/Status.svelte';
  import Notice from '#lib/components/Notice.svelte';
  let repos = $state<Repository[]>([]);
  let search = $state('');
  let error = $state('');
  let message = $state('');
  let loading = $state(true);
  let busy = $state('');
  let dialog = $state(false);
  let selected = $state('');
  let pulls = $state<PullRequest[]>([]);
  let pullsLoading = $state(false);
  let pullError = $state('');
  let installUrl = $state('');
  let ready = $state(false);
  const filtered = $derived(
    repos.filter((repo) => repo.full_name.toLowerCase().includes(search.toLowerCase())),
  );
  async function load() {
    try {
      repos = await api.repos();
      error = '';
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      loading = false;
    }
  }
  onMount(() => {
    ready = true;
    void api
      .overview()
      .then((overview) => (installUrl = overview.app.install_url))
      .catch((cause) => (error = errorMessage(cause)));
  });
  $effect(() => {
    $live.repos;
    if (ready) void load();
  });
  async function toggle(repo: Repository, enabled: boolean) {
    busy = repo.full_name;
    try {
      await api.enable(repo.full_name, enabled);
      repo.enabled = enabled;
      message = `${repo.full_name}: reviews ${enabled ? 'enabled' : 'paused'}.`;
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = '';
    }
  }
  async function index(repo: string) {
    busy = repo;
    try {
      await api.index(repo);
      message = `Index queued for ${repo}.`;
      await load();
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = '';
    }
  }
  async function choosePull(repo: string) {
    selected = repo;
    pulls = [];
    pullError = '';
    dialog = true;
    pullsLoading = true;
    try {
      pulls = await api.pulls(repo);
    } catch (cause) {
      pullError = errorMessage(cause);
    } finally {
      pullsLoading = false;
    }
  }
  async function review(number: number) {
    busy = selected;
    try {
      await api.review(selected, number);
      message = `Review queued for ${selected} #${number}.`;
      dialog = false;
    } catch (cause) {
      pullError = errorMessage(cause);
    } finally {
      busy = '';
    }
  }
</script>

<svelte:head><title>Repositories · Revera</title></svelte:head>
<div class="page-header">
  <div>
    <h1>Repositories</h1>
    <p>Choose what gets reviewed and keep repository indexes up to date.</p>
  </div>
  {#if installUrl}<a href={installUrl} class="button" target="_blank" rel="noreferrer"
      >Manage installation <ArrowUpRight size={14} /></a
    >{/if}
</div>
<div class="filters">
  <div class="search-field">
    <label class="sr-only" for="repo-search">Search repositories</label><Search size={15} /><input
      id="repo-search"
      type="search"
      bind:value={search}
      placeholder="Search repositories…"
    />
  </div>
  <span class="result-count">{filtered.length} repositories</span>
</div>
{#if error}<Notice message={error} retry={load} />{/if}{#if message}<Notice
    {message}
    kind="success"
  />{/if}
{#if filtered.length}<div class="table-scroll">
    <table style="min-width: 760px">
      <thead
        ><tr
          ><th style="width: 30%">Repository</th><th>Reviews</th><th>Search index</th><th
            >Configuration</th
          ><th>Last review</th><th><span class="sr-only">Actions</span></th></tr
        ></thead
      ><tbody
        >{#each filtered as repo}<tr
            ><td
              ><a href={repo.html_url} class="row-title" target="_blank" rel="noreferrer"
                >{repo.full_name}</a
              >
              <div class="row-meta">
                {#if repo.private}<LockKeyhole size={10} />Private{:else}Public{/if}<span>·</span
                >{repo.default_branch}
              </div></td
            ><td
              ><div class="actions">
                <Toggle
                  checked={repo.enabled}
                  label={`Enable reviews for ${repo.full_name}`}
                  disabled={busy === repo.full_name}
                  onchange={(enabled) => void toggle(repo, enabled)}
                /><span class="muted">{repo.enabled ? 'On' : 'Paused'}</span>
              </div></td
            ><td
              ><Status
                status={repo.index.state}
                label={repo.index.state === 'none' ? 'Not indexed' : repo.index.state}
              />{#if repo.index.updated_at}<div class="row-meta">
                  <time
                    datetime={repo.index.updated_at}
                    title={new Date(repo.index.updated_at).toLocaleString()}
                    >{relative(repo.index.updated_at)}</time
                  >
                </div>{/if}</td
            ><td
              >{#if repo.has_override}<span
                  class="badge"
                  title="Uses revera.yaml from the repository base branch">Repository override</span
                >{:else}<span class="muted">Server default</span>{/if}</td
            ><td
              >{#if repo.last_job}<a href={`/reviews/${repo.last_job.id}`}
                  ><Status status={repo.last_job.status} />
                  <div class="row-meta">
                    #{repo.last_job.pr} · {relative(repo.last_job.queued_at)}
                  </div></a
                >{:else}<span class="muted">—</span>{/if}</td
            ><td
              ><DropdownMenu.Root
                ><DropdownMenu.Trigger
                  class="button ghost icon small"
                  aria-label={`Actions for ${repo.full_name}`}
                  ><MoreHorizontal size={17} /></DropdownMenu.Trigger
                ><DropdownMenu.Portal
                  ><DropdownMenu.Content class="menu" sideOffset={4} align="end"
                    ><DropdownMenu.Item
                      class="menu-item"
                      disabled={busy === repo.full_name || repo.index.state === 'building'}
                      onSelect={() => void index(repo.full_name)}
                      ><RefreshCw size={14} />Index now</DropdownMenu.Item
                    ><DropdownMenu.Item
                      class="menu-item"
                      onSelect={() => void choosePull(repo.full_name)}
                      ><GitPullRequest size={14} />Review a pull request</DropdownMenu.Item
                    ></DropdownMenu.Content
                  ></DropdownMenu.Portal
                ></DropdownMenu.Root
              ></td
            ></tr
          >{/each}</tbody
      >
    </table>
  </div>
  <p class="helper" style="margin-top: 16px">
    Repository overrides are read from the base branch, never from the pull request.
  </p>
{:else if loading}<div class="loading" role="status">Loading repositories…</div>{:else}<div
    class="empty-state"
  >
    {search ? 'No repositories match your search.' : 'No repositories are installed yet.'}<br
    />{#if search}<button class="button" onclick={() => (search = '')}>Clear search</button
      >{:else if installUrl}<a class="button" href={installUrl}>Install on repositories</a>{/if}
  </div>{/if}

<Dialog.Root bind:open={dialog}
  ><Dialog.Portal
    ><Dialog.Overlay class="dialog-overlay" /><Dialog.Content class="dialog"
      ><Dialog.Title class="dialog-title"><h2>Review a pull request</h2></Dialog.Title
      ><Dialog.Description class="dialog-description"
        >Choose an open pull request in {selected}.</Dialog.Description
      >{#if pullError}<Notice message={pullError} />{/if}{#if pullsLoading}<p class="loading">
          Loading pull requests…
        </p>{:else}{#each pulls as pull}<button
            class="pull-option"
            disabled={busy === selected}
            onclick={() => void review(pull.number)}
            ><span class="row-title">#{pull.number} {pull.title}</span><span class="row-meta"
              >{pull.author}{#if pull.draft}<span class="badge">Draft</span>{/if}<span>·</span
              >{relative(pull.updated_at)}</span
            ></button
          >{:else}<p class="muted">There are no open pull requests.</p>{/each}{/if}
      <div class="actions"><Dialog.Close class="button">Cancel</Dialog.Close></div></Dialog.Content
    ></Dialog.Portal
  ></Dialog.Root
>
