<script lang="ts">
  import '../app.css';
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { goto, afterNavigate } from '$app/navigation';
  import { DropdownMenu } from 'bits-ui';
  import {
    LayoutDashboard,
    GitPullRequest,
    FolderGit2,
    Settings,
    ChevronDown,
    LogOut,
    ArrowUpRight,
    PanelLeftClose,
    PanelLeftOpen,
    Monitor,
    Sun,
    Moon,
    Check,
  } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import { connectEvents } from '#lib/events';
  import { theme, setTheme, type ThemeChoice } from '#lib/theme.svelte';
  import type { Session } from '#lib/types';
  import Notice from '#lib/components/Notice.svelte';

  let { children } = $props();
  let session = $state<Session>();
  let error = $state('');
  let appName = $state('Revera');
  let collapsed = $state(false);
  onMount(() => {
    try {
      collapsed = localStorage.getItem('revera-sidebar') === 'collapsed';
    } catch {
      /* Storage may be disabled. */
    }
  });
  function toggleSidebar() {
    collapsed = !collapsed;
    try {
      localStorage.setItem('revera-sidebar', collapsed ? 'collapsed' : 'expanded');
    } catch {
      /* Storage may be disabled. */
    }
  }
  const themes = [
    { value: 'system', label: 'System', icon: Monitor },
    { value: 'light', label: 'Light', icon: Sun },
    { value: 'dark', label: 'Dark', icon: Moon },
  ] as const;
  const currentTheme = $derived(themes.find((item) => item.value === theme.choice)!);
  const nav = [
    { href: '/', name: 'Overview', icon: LayoutDashboard },
    { href: '/reviews', name: 'Reviews', icon: GitPullRequest },
    { href: '/repositories', name: 'Repositories', icon: FolderGit2 },
    { href: '/settings', name: 'Settings', icon: Settings },
  ];
  const isSetup = $derived(page.url.pathname === '/setup');
  const currentPage = $derived(nav.find((item) => isActive(item.href))?.name);
  function isActive(href: string) {
    return href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href);
  }
  async function load() {
    try {
      session = await api.session();
      error = '';
      const settingsRecovery =
        page.url.pathname === '/settings' &&
        session.authenticated &&
        session.setup.stage === 'configure';
      if (!isSetup && session.setup.stage !== 'done' && !settingsRecovery) await goto('/setup');
      if (session.authenticated) appName = (await api.overview()).app.name;
    } catch (cause) {
      error = errorMessage(cause);
    }
  }
  async function logout() {
    try {
      await api.logout();
      session = await api.session();
      await goto('/');
    } catch (cause) {
      error = errorMessage(cause);
    }
  }
  afterNavigate(() => {
    void load();
  });
  $effect(() => {
    if (session?.authenticated) return connectEvents();
  });
</script>

<svelte:head
  ><title>Revera</title><meta
    name="description"
    content="Manage your Revera review server."
  /></svelte:head
>

{#if isSetup}
  {@render children()}
{:else if error && !session}
  <main class="standalone">
    <div class="wordmark"><span class="brand-mark">R</span> Revera</div>
    <Notice message={error} retry={load} />
  </main>
{:else if !session}
  <main class="standalone"><p class="muted" role="status">Loading Revera…</p></main>
{:else if !session.authenticated}
  <main class="standalone">
    <div class="wordmark"><span class="brand-mark">R</span> Revera</div>
    <h1>Sign in to your dashboard</h1>
    <p class="muted">Use the GitHub account your administrator has approved.</p>
    <a class="button primary" href="/auth/login">Sign in with GitHub <ArrowUpRight size={16} /></a>
  </main>
{:else}
  <div class="app-shell" class:collapsed>
    <aside class="sidebar" aria-label="Main navigation">
      <a class="wordmark" href="/" aria-label={`${appName} overview`}
        ><span class="brand-mark">R</span><span class="sidebar-label">{appName}</span></a
      >
      <div class="sidebar-section sidebar-label">Workspace</div>
      <nav>
        {#each nav as item}<a
            href={item.href}
            class:active={isActive(item.href)}
            aria-current={isActive(item.href) ? 'page' : undefined}
            title={item.name}
            ><item.icon size={18} strokeWidth={1.7} /><span class="sidebar-label">{item.name}</span
            ></a
          >{/each}
      </nav>
      <div class="sidebar-bottom">
        <button
          class="sidebar-collapse"
          onclick={toggleSidebar}
          aria-label={collapsed ? 'Expand navigation' : 'Collapse navigation'}
          >{#if collapsed}<PanelLeftOpen size={17} />{:else}<PanelLeftClose size={17} /><span
              class="sidebar-label">Collapse sidebar</span
            >{/if}</button
        >
        <DropdownMenu.Root>
          <DropdownMenu.Trigger class="user-menu"
            ><img src={session.user?.avatar_url} alt="" width="30" height="30" /><span
              class="sidebar-label user-copy"
              ><strong>{session.user?.login}</strong><span>Administrator</span></span
            ><ChevronDown class="sidebar-label" size={14} /></DropdownMenu.Trigger
          >
          <DropdownMenu.Portal
            ><DropdownMenu.Content class="menu" side="top" align="start" sideOffset={8}
              ><DropdownMenu.Item class="menu-item" onSelect={() => void logout()}
                ><LogOut size={15} />Sign out</DropdownMenu.Item
              ></DropdownMenu.Content
            ></DropdownMenu.Portal
          >
        </DropdownMenu.Root>
      </div>
    </aside>
    <div class="workspace">
      <header class="workspace-bar">
        <span>{appName}<span class="breadcrumb-slash">/</span>{currentPage}</span>
        <div class="actions">
          <DropdownMenu.Root>
            <DropdownMenu.Trigger class="button ghost icon" aria-label="Theme" title="Theme">
              <currentTheme.icon size={17} />
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content class="menu" align="end" sideOffset={4}>
                <DropdownMenu.RadioGroup
                  value={theme.choice}
                  onValueChange={(value) => setTheme(value as ThemeChoice)}
                  aria-label="Theme"
                >
                  {#each themes as item}
                    <DropdownMenu.RadioItem class="menu-item" value={item.value}>
                      {#snippet children({ checked })}
                        <item.icon size={15} />{item.label}
                        {#if checked}<Check size={15} style="margin-left: auto" />{/if}
                      {/snippet}
                    </DropdownMenu.RadioItem>
                  {/each}
                </DropdownMenu.RadioGroup>
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu.Root>
          <a href="https://github.com/VeraTools/revera" target="_blank" rel="noreferrer"
            >Revera on GitHub <ArrowUpRight size={13} /></a
          >
        </div>
      </header>
      <main class="main-content">
        {#if error}<Notice message={error} />{/if}{@render children()}
      </main>
    </div>
  </div>
{/if}
