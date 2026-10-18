<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { afterNavigate, goto, replaceState } from '$app/navigation';
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
  import { createNotices } from '#lib/notices.svelte';
  import type {
    Repository,
    PullRequest,
    Trigger,
    ReviewMode,
    Installations,
    Installation,
  } from '#lib/types';
  import { modes, triggers } from '#lib/config.svelte';
  import SelectField from '#lib/components/SelectField.svelte';
  import ReviewModeField from '#lib/components/ReviewModeField.svelte';
  import Status from '#lib/components/Status.svelte';
  import Notice from '#lib/components/Notice.svelte';
  import RepoFixSettings from '#lib/components/RepoFixSettings.svelte';
  let fixRepo = $state('');
  let repos = $state<Repository[]>([]);
  let search = $state('');
  let triggerFilter = $state('');
  let activityFilter = $state('');
  let sort = $state('');
  let selection = $state<string[]>([]);
  let bulkTrigger = $state('');
  let bulkMode = $state('');
  let defaultTrigger = $state<Trigger>('mention');
  let defaultMode = $state<ReviewMode>('standard');
  let reviewMode = $state<ReviewMode | ''>('');
  let selectedMode = $state<ReviewMode>('standard');
  const notice = createNotices();
  let loading = $state(true);
  let busy = $state('');
  let dialog = $state(false);
  let selected = $state('');
  let pulls = $state<PullRequest[]>([]);
  let pullsLoading = $state(false);
  let pullError = $state('');
  let installUrl = $state('');
  let accounts = $state<Installations>();
  let accountBusy = $state(false);
  let accountError = $state('');
  const accountStatus = {
    allowed: 'Allowed',
    awaiting_approval: 'Awaiting Revera approval',
    permission_update: 'Permission update required',
    suspended: 'Suspended',
    not_installed: 'Not installed',
  };
  async function loadAccounts(refresh = false) {
    accountBusy = true;
    try {
      accounts = refresh ? await api.reconcileInstallations() : await api.installations();
      installUrl = accounts.install_url;
      accountError = '';
      if (refresh) await load();
    } catch (cause) {
      accountError = errorMessage(cause);
    } finally {
      accountBusy = false;
    }
  }
  async function changeOwner(account: Installation, approve: boolean) {
    if (
      !approve &&
      !window.confirm(
        `Revoke ${account.account.login}? Queued and running jobs will be cancelled and its repositories removed.`,
      )
    )
      return;
    accountBusy = true;
    try {
      const result = await api.owner(account.account.login, approve);
      await loadAccounts();
      await load();
      if (result.warning) accountError = result.warning;
    } catch (cause) {
      accountError = errorMessage(cause);
    } finally {
      accountBusy = false;
    }
  }
  let ready = $state(false);
  const timestamp = (date?: string) => (date ? Date.parse(date) || 0 : 0);
  const activity = (repo: Repository) =>
    Math.max(timestamp(repo.pushed_at), timestamp(repo.last_job?.queued_at));
  const activityOptions = [
    { value: '', label: 'Any activity' },
    { value: '7d', label: 'Active in the last 7 days' },
    { value: '30d', label: 'Active in the last 30 days' },
    { value: 'inactive', label: 'No activity in 30 days' },
  ];
  const sortOptions = [
    { value: '', label: 'Sort: Recent activity' },
    { value: 'name', label: 'Sort: Name' },
    { value: 'review', label: 'Sort: Last review' },
    { value: 'prs', label: 'Sort: Open pull requests' },
    { value: 'reviewed', label: 'Sort: Most reviewed' },
  ];
  const filtered = $derived(
    repos
      .filter((repo) => {
        const age = Date.now() - activity(repo);
        return (
          repo.full_name.toLowerCase().includes(search.toLowerCase()) &&
          (!triggerFilter || repo.trigger === triggerFilter) &&
          (!activityFilter ||
            (activityFilter === 'inactive'
              ? age > 30 * 86400000
              : age <= (activityFilter === '7d' ? 7 : 30) * 86400000))
        );
      })
      .sort((a, b) => {
        const difference =
          sort === 'name'
            ? 0
            : sort === 'review'
              ? timestamp(b.last_review?.queued_at) - timestamp(a.last_review?.queued_at)
              : sort === 'prs'
                ? (b.open_prs ?? -1) - (a.open_prs ?? -1)
                : sort === 'reviewed'
                  ? b.review_count - a.review_count
                  : activity(b) - activity(a);
        return difference || a.full_name.localeCompare(b.full_name);
      }),
  );
  // Counts cover the full inventory, so searching doesn't move the segments.
  const segments = $derived([
    { value: '', label: 'All', count: repos.length },
    ...['auto', 'mention', 'off'].map((value) => ({
      value,
      label: value[0].toUpperCase() + value.slice(1),
      count: repos.filter((repo) => repo.trigger === value).length,
    })),
  ]);
  function readQuery() {
    const params = page.url.searchParams;
    search = params.get('q') || '';
    triggerFilter = triggers.some((item) => item.value === params.get('trigger'))
      ? params.get('trigger')!
      : '';
    activityFilter = activityOptions.some((item) => item.value === params.get('active'))
      ? params.get('active')!
      : '';
    sort = sortOptions.some((item) => item.value === params.get('sort')) ? params.get('sort')! : '';
    ready = true;
  }
  afterNavigate(readQuery);
  $effect(() => {
    const values = { q: search, trigger: triggerFilter, active: activityFilter, sort };
    if (!ready) return;
    const url = new URL(window.location.href);
    for (const [key, value] of Object.entries(values)) {
      if (value) url.searchParams.set(key, value);
      else url.searchParams.delete(key);
    }
    if (url.href !== window.location.href) replaceState(url, {});
  });
  $effect(() => {
    const visible = new Set(filtered.map((repo) => repo.full_name));
    const kept = selection.filter((name) => visible.has(name));
    if (kept.length !== selection.length) selection = kept;
  });
  function selectRow(name: string, checked: boolean) {
    selection = checked ? [...selection, name] : selection.filter((item) => item !== name);
  }
  const retryIn = (date: string) =>
    `${Math.max(1, Math.ceil((timestamp(date) - Date.now()) / 60000))}m`;
  const triggerOptions = $derived([
    {
      value: '',
      label: `Default (${triggers.find((item) => item.value === defaultTrigger)?.label})`,
    },
    ...triggers,
  ]);
  const modeOptions = $derived([
    { value: '', label: `Default (${modes.find((item) => item.value === defaultMode)?.label})` },
    ...modes,
  ]);
  async function load() {
    try {
      const [items, defaults] = await Promise.all([api.repos(), api.serverSettings()]);
      repos = items;
      defaultTrigger = defaults.default_trigger;
      defaultMode = defaults.default_mode;
      notice.state.error = '';
    } catch (cause) {
      notice.failure(errorMessage(cause));
    } finally {
      loading = false;
    }
  }
  onMount(() => {
    readQuery();
    void loadAccounts(
      page.url.searchParams.has('installation_id') || page.url.searchParams.has('setup_action'),
    );
  });
  $effect(() => {
    const revision = $live.repos;
    if (ready) {
      void load();
      if (revision) void loadAccounts();
    }
  });
  async function update(repo: Repository, field: 'trigger' | 'mode', value: string) {
    busy = repo.full_name;
    try {
      await api.repoSettings(repo.full_name, { [field]: value || null });
      notice.success(`${repo.full_name}: ${field} saved.`);
      await load();
    } catch (cause) {
      notice.failure(errorMessage(cause));
    } finally {
      busy = '';
    }
  }
  async function index(repo: string) {
    busy = repo;
    try {
      await api.index(repo);
      notice.success(`Index queued for ${repo}.`);
      await load();
    } catch (cause) {
      notice.failure(errorMessage(cause));
    } finally {
      busy = '';
    }
  }
  async function bulkSettings(field: 'trigger' | 'mode', value: string) {
    if (!value) return;
    busy = 'bulk';
    try {
      const result = await api.bulkRepoSettings([...selection], {
        [field]: value === 'default' ? null : value,
      });
      const label =
        value === 'default'
          ? `Default (${field === 'trigger' ? defaultTrigger : defaultMode})`
          : value[0].toUpperCase() + value.slice(1);
      notice.success(
        `${field === 'trigger' ? 'Trigger' : 'Mode'} set to ${label} for ${result.updated} ${result.updated === 1 ? 'repository' : 'repositories'}.`,
      );
      await load();
    } catch (cause) {
      notice.failure(errorMessage(cause));
    } finally {
      busy = '';
      bulkTrigger = '';
      bulkMode = '';
    }
  }
  async function bulkIndex() {
    const selectedRepos = repos.filter((repo) => selection.includes(repo.full_name));
    busy = 'bulk';
    let queued = 0;
    const off = selectedRepos.filter((repo) => repo.trigger === 'off').length;
    const building = selectedRepos.filter(
      (repo) => repo.trigger !== 'off' && repo.index.state === 'building',
    ).length;
    try {
      for (const repo of selectedRepos) {
        if (repo.trigger === 'off' || repo.index.state === 'building') continue;
        await api.index(repo.full_name);
        queued++;
      }
      notice.success(
        `Index queued for ${queued} ${queued === 1 ? 'repository' : 'repositories'}.${off ? ` ${off} skipped (reviews off).` : ''}${building ? ` ${building} skipped (index building).` : ''}`,
      );
      await load();
    } catch (cause) {
      const failure = `Index queued for ${queued} repositories before an error: ${errorMessage(cause)}`;
      await load();
      notice.failure(failure);
    } finally {
      busy = '';
    }
  }
  async function choosePull(repo: Repository) {
    selected = repo.full_name;
    selectedMode = repo.mode;
    reviewMode = '';
    pulls = [];
    pullError = '';
    dialog = true;
    pullsLoading = true;
    try {
      pulls = await api.pulls(repo.full_name);
    } catch (cause) {
      pullError = errorMessage(cause);
    } finally {
      pullsLoading = false;
    }
  }
  async function review(number: number) {
    busy = selected;
    try {
      await api.review(selected, number, reviewMode || null);
      notice.success(`Review queued for ${selected} #${number}.`);
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
      >Add repositories or organization <ArrowUpRight size={14} /></a
    >{/if}
</div>
<section class="connected-accounts" aria-label="Connected accounts">
  <div class="section-heading">
    <h2>Connected accounts</h2>
    <button class="button small" disabled={accountBusy} onclick={() => void loadAccounts(true)}
      ><RefreshCw size={13} />{accountBusy ? 'Refreshing…' : 'Refresh'}</button
    >
  </div>
  <p class="helper">
    GitHub organization permissions decide who can install the app. Revera administrators separately
    approve which owners this deployment may review. Installing alone never grants access.
  </p>
  {#if accountError}<Notice message={accountError} retry={() => loadAccounts(true)} />{/if}
  {#if accounts}
    {#each accounts.installations as account (account.account.login)}
      <div class="account-row" data-account={account.account.login}>
        <div>
          <strong>{account.account.login}</strong>
          <span class="badge"
            >{account.error ? 'Access unavailable' : accountStatus[account.status]}</span
          >
          <div class="row-meta">
            {account.account.type ||
              'GitHub account'}{#if account.repo_count !== undefined}{' · '}{account.repo_count} repositories{/if}{#if account.repository_selection}{' · '}{account.repository_selection ===
              'all'
                ? 'All repositories'
                : 'Selected repositories'}{/if}
          </div>
          {#if account.error}<p class="helper">{account.error}</p>{/if}
        </div>
        <div class="actions">
          {#if account.html_url}<a
              class="button small"
              href={account.html_url}
              target="_blank"
              rel="noreferrer"
              >{account.id ? 'Manage on GitHub' : 'Install on GitHub'}<ArrowUpRight size={13} /></a
            >{/if}
          {#if account.allowed}<button
              class="button small"
              disabled={accountBusy}
              onclick={() => void changeOwner(account, false)}
              aria-label={`Revoke ${account.account.login}`}>Revoke</button
            >
          {:else}<button
              class="button small"
              disabled={accountBusy}
              onclick={() => void changeOwner(account, true)}
              aria-label={`Approve ${account.account.login}`}>Approve</button
            >{/if}
        </div>
      </div>
    {:else}<p class="helper">
        No accounts installed yet. Add an account using GitHub’s installation page.
      </p>{/each}
    <details>
      <summary>Connecting another organization or enabling write permissions</summary>
      <p class="helper">
        Private GitHub Apps can only be installed on their owning account. For an existing
        registration, open <a href={accounts.advanced_url} target="_blank" rel="noreferrer"
          >GitHub app settings → Advanced → Make public</a
        > if available. Changing a setup manifest cannot change an existing registration. This does not
        make your dashboard public.
      </p>
      <p class="helper">
        For approved fix delivery, change Contents to Read and write in <a
          href={accounts.settings_url}
          target="_blank"
          rel="noreferrer">Permissions &amp; events</a
        >, then have each installation owner accept the new permissions. Add this dashboard’s
        <code>/repositories</code> URL as the app’s Setup URL for the return flow. Review-only and patch-only
        setups can keep Contents read.
      </p>
    </details>
  {/if}
</section>
<div class="filters">
  <div class="search-field">
    <label class="sr-only" for="repo-search">Search repositories</label><Search size={15} /><input
      id="repo-search"
      type="search"
      bind:value={search}
      placeholder="Search repositories…"
    />
  </div>
  <div class="segmented" role="group" aria-label="Filter by trigger">
    {#each segments as segment}<button
        aria-pressed={triggerFilter === segment.value}
        onclick={() => (triggerFilter = segment.value)}
        >{segment.label} <span>{segment.count}</span></button
      >{/each}
  </div>
  <SelectField
    id="repo-activity"
    label="Activity"
    bind:value={activityFilter}
    options={activityOptions}
  />
  <SelectField id="repo-sort" label="Sort repositories" bind:value={sort} options={sortOptions} />
  <span class="result-count">{filtered.length} repositories</span>
</div>
{#if notice.state.error}<Notice
    message={notice.state.error}
    retry={load}
  />{/if}{#if notice.state.message}<Notice message={notice.state.message} kind="success" />{/if}
{#if filtered.length}<div class="table-scroll">
    <table class="repos-table" style="min-width: 940px">
      <thead
        ><tr
          ><th class="selection-cell"
            ><input
              type="checkbox"
              aria-label="Select all visible repositories"
              checked={selection.length === filtered.length}
              indeterminate={selection.length > 0 && selection.length < filtered.length}
              onchange={(event) =>
                (selection = event.currentTarget.checked
                  ? filtered.map((repo) => repo.full_name)
                  : [])}
            /></th
          ><th>Repository</th><th>Trigger</th><th>Mode</th><th>Last review</th><th>Last push</th><th
            >Search index</th
          ><th><span class="sr-only">Actions</span></th></tr
        ></thead
      ><tbody
        >{#each filtered as repo (repo.full_name)}<tr
            class:selected-row={selection.includes(repo.full_name)}
            ><td class="selection-cell"
              ><input
                type="checkbox"
                aria-label={`Select ${repo.full_name}`}
                checked={selection.includes(repo.full_name)}
                onchange={(event) => selectRow(repo.full_name, event.currentTarget.checked)}
              /></td
            ><td
              ><a href={repo.html_url} class="row-title" target="_blank" rel="noreferrer"
                >{repo.full_name}</a
              >
              <div class="row-meta">
                {#if repo.private}<LockKeyhole size={10} />Private{:else}Public{/if}<span>·</span
                >{repo.default_branch}{#if repo.open_prs && repo.open_prs > 0}<span>·</span
                  >{repo.open_prs} open PRs{/if}
                {#if repo.has_override}<span
                    class="badge"
                    title="Uses revera.yaml from the repository base branch">revera.yaml</span
                  >{/if}
              </div></td
            ><td
              ><SelectField
                id={`trigger-${repo.full_name}`}
                label={`Trigger for ${repo.full_name}`}
                value={repo.trigger_override || ''}
                options={triggerOptions}
                disabled={!!busy}
                onchange={(value) => void update(repo, 'trigger', value)}
              /></td
            ><td
              ><SelectField
                id={`mode-${repo.full_name}`}
                label={`Mode for ${repo.full_name}`}
                value={repo.mode_override || ''}
                options={modeOptions}
                disabled={!!busy}
                onchange={(value) => void update(repo, 'mode', value)}
              /></td
            ><td
              >{#if repo.last_review}<a href={`/reviews/${repo.last_review.id}`}
                  ><Status status={repo.last_review.status} />
                  <div class="row-meta">
                    #{repo.last_review.pr} · {relative(
                      repo.last_review.queued_at,
                    )}{#if repo.review_count > 1}{` · ${repo.review_count} reviews`}{/if}
                  </div></a
                >
              {:else}<span class="muted">—</span>{/if}</td
            >
            <td class="muted nowrap"
              >{#if repo.pushed_at}<time
                  datetime={repo.pushed_at}
                  title={new Date(repo.pushed_at).toLocaleString()}>{relative(repo.pushed_at)}</time
                >{:else}—{/if}</td
            >
            <td
              ><Status
                status={repo.index.state}
                label={repo.index.state === 'none' ? 'Not indexed' : repo.index.state}
              />{#if repo.index.updated_at}<div class="row-meta">
                  <time
                    datetime={repo.index.updated_at}
                    title={new Date(repo.index.updated_at).toLocaleString()}
                    >{relative(repo.index.updated_at)}</time
                  >
                </div>{/if}{#if repo.index.retry_at}<div
                  class="row-meta"
                  title={new Date(repo.index.retry_at).toLocaleString()}
                >
                  Retry in {retryIn(repo.index.retry_at)}
                </div>{/if}</td
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
                      disabled={!!busy || repo.index.state === 'building' || repo.trigger === 'off'}
                      onSelect={() => void index(repo.full_name)}
                      ><RefreshCw size={14} />Index now</DropdownMenu.Item
                    ><DropdownMenu.Item
                      class="menu-item"
                      disabled={!!busy || repo.trigger === 'off'}
                      onSelect={() => void choosePull(repo)}
                      ><GitPullRequest size={14} />Review a pull request</DropdownMenu.Item
                    ><DropdownMenu.Item
                      class="menu-item"
                      onSelect={() =>
                        void goto(`/reviews?repo=${encodeURIComponent(repo.full_name)}`)}
                      ><ArrowUpRight size={14} />View reviews</DropdownMenu.Item
                    ><DropdownMenu.Item
                      class="menu-item"
                      onSelect={() => (fixRepo = repo.full_name)}>Fix settings</DropdownMenu.Item
                    ></DropdownMenu.Content
                  ></DropdownMenu.Portal
                ></DropdownMenu.Root
              ></td
            ></tr
          >{/each}</tbody
      >
    </table>
  </div>
  {#if repos.some((repo) => repo.has_override)}<p class="helper" style="margin-top: 16px">
      Repository overrides are read from the base branch, never from the pull request.
    </p>{/if}
{:else if loading}<div class="loading" role="status">Loading repositories…</div>{:else}<div
    class="empty-state"
  >
    {search || triggerFilter || activityFilter
      ? 'No repositories match your filters.'
      : 'No repositories are installed yet.'}<br
    />{#if search || triggerFilter || activityFilter}<button
        class="button"
        onclick={() => {
          search = '';
          triggerFilter = '';
          activityFilter = '';
        }}>Clear filters</button
      >{:else if installUrl}<a class="button" href={installUrl}>Install on repositories</a>{/if}
  </div>{/if}

{#if selection.length}<div class="save-bar bulk-bar" aria-label="Bulk repository actions">
    <strong>{selection.length} selected</strong>
    <div class="actions">
      <SelectField
        id="bulk-trigger"
        label="Set trigger for selected repositories"
        bind:value={bulkTrigger}
        options={[
          { value: '', label: 'Set trigger…', disabled: true },
          ...triggerOptions.map((item) => ({ ...item, value: item.value || 'default' })),
        ]}
        disabled={!!busy}
        onchange={(value) => void bulkSettings('trigger', value)}
      />
      <SelectField
        id="bulk-mode"
        label="Set mode for selected repositories"
        bind:value={bulkMode}
        options={[
          { value: '', label: 'Set mode…', disabled: true },
          ...modeOptions.map((item) => ({ ...item, value: item.value || 'default' })),
        ]}
        disabled={!!busy}
        onchange={(value) => void bulkSettings('mode', value)}
      />
      <button class="button small" disabled={!!busy} onclick={bulkIndex}>Index now</button>
      <button class="button small" disabled={!!busy} onclick={() => (selection = [])}>Clear</button>
    </div>
  </div>{/if}

<Dialog.Root bind:open={dialog}
  ><Dialog.Portal
    ><Dialog.Overlay class="dialog-overlay" /><Dialog.Content class="dialog"
      ><Dialog.Title class="dialog-title"><h2>Review a pull request</h2></Dialog.Title
      ><Dialog.Description class="dialog-description"
        >Choose an open pull request in {selected}.</Dialog.Description
      ><ReviewModeField
        id="manual-review-mode"
        bind:value={reviewMode}
        defaultMode={selectedMode}
      />
      {#if pullError}<Notice message={pullError} />{/if}{#if pullsLoading}<p class="loading">
          Loading pull requests…
        </p>{:else}{#each pulls as pull}<button
            class="pull-option"
            disabled={busy === selected}
            onclick={() => void review(pull.number)}
            ><span class="row-title">#{pull.number} {pull.title}</span><span class="row-meta"
              >{pull.author}{#if pull.draft}<span class="badge">Draft</span>{/if}<span>·</span
              >{relative(pull.updated_at)}</span
            ><span class="row-meta"
              >{#if pull.last_job}<Status status={pull.last_job.status} /> · {relative(
                  pull.last_job.queued_at,
                )}{:else}Not reviewed{/if}</span
            ></button
          >{:else}<p class="muted">There are no open pull requests.</p>{/each}{/if}
      <div class="actions"><Dialog.Close class="button">Cancel</Dialog.Close></div></Dialog.Content
    ></Dialog.Portal
  ></Dialog.Root
>

{#if fixRepo}{#key fixRepo}<RepoFixSettings
      repo={fixRepo}
      onclose={() => (fixRepo = '')}
    />{/key}{/if}

<style>
  .connected-accounts {
    margin-bottom: 24px;
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 16px;
  }
  .section-heading,
  .account-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
  }
  .account-row {
    padding: 12px 0;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .connected-accounts details {
    margin-top: 12px;
  }
</style>
