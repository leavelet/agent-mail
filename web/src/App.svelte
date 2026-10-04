<script lang="ts">
  import { onMount } from 'svelte';
  import { store } from './lib/store.svelte';
  import { router, go } from './lib/router.svelte';
  import { applyLang, applyTheme, startClock, t, ui } from './lib/ui.svelte';
  import { typing } from './lib/keys';
  import Login from './components/Login.svelte';
  import TopBar from './components/TopBar.svelte';
  import NavBar from './components/NavBar.svelte';
  import HelpOverlay from './components/HelpOverlay.svelte';
  import OrgPage from './pages/OrgPage.svelte';
  import NeedsPage from './pages/NeedsPage.svelte';
  import InboxPage from './pages/InboxPage.svelte';
  import ThreadPage from './pages/ThreadPage.svelte';
  import ArchivePage from './pages/ArchivePage.svelte';
  import ComposePage from './pages/ComposePage.svelte';
  import AdminSeats from './pages/admin/AdminSeats.svelte';
  import AdminLists from './pages/admin/AdminLists.svelte';
  import AdminLinks from './pages/admin/AdminLinks.svelte';
  import AdminTokens from './pages/admin/AdminTokens.svelte';
  import AdminSettings from './pages/admin/AdminSettings.svelte';
  import RegisterPage from './pages/RegisterPage.svelte';
  import OperatorPage from './pages/OperatorPage.svelte';

  const registering = globalThis.location?.pathname === '/register';

  onMount(() => {
    applyTheme(ui.theme);
    applyLang(ui.lang);
    void store.check();
    return startClock();
  });

  const parts = $derived(router.route.parts);
  const query = $derived(router.route.query);
  const section = $derived(parts[0] ?? 'org');

  let gPending = false;
  function onKey(e: KeyboardEvent) {
    if (store.auth !== 'in' || typing(e)) return;
    if (e.key === '?') {
      ui.helpOpen = !ui.helpOpen;
      e.preventDefault();
      return;
    }
    if (ui.helpOpen) return;
    if (gPending) {
      gPending = false;
      const target = { n: 'needs', o: 'org', i: 'inbox', a: 'archive' }[e.key];
      if (target) {
        go([target]);
        e.preventDefault();
      }
      return;
    }
    if (e.key === 'g') {
      gPending = true;
      setTimeout(() => (gPending = false), 1200);
      return;
    }
    if (e.defaultPrevented) return;
    if (e.key === 'c' && section !== 'compose') {
      // In the org view, write to the selected seat.
      const seat = (section === 'org' || section === 'seat') && parts[1] ? store.seatMap.get(parts[1]) : undefined;
      go(['compose'], seat?.kind === 'agent' ? { to: seat.name } : undefined);
      e.preventDefault();
    } else if (e.key === '/') {
      const el = document.querySelector<HTMLInputElement>('[data-search]');
      e.preventDefault();
      if (el) el.focus();
      else go(['archive']);
    }
  }
</script>

<svelte:window onkeydown={onKey} />

{#if store.auth === 'checking'}
  <div class="empty">{t('app.loading')}</div>
{:else if store.auth === 'out' && registering}
  <RegisterPage />
{:else if store.auth === 'out'}
  <Login />
{:else if store.auth === 'operator'}
  <OperatorPage />
{:else}
  <div class="shell">
    <TopBar />
    <NavBar />
    <main class="main" id="main">
      {#if store.loadError}
        <div class="banner error" role="alert">
          {t('app.error', { msg: store.loadError })}
          <button class="small" onclick={() => store.check()}>{t('app.retry')}</button>
        </div>
      {/if}
      {#if section === 'org' || section === 'seat'}
        <OrgPage selected={parts[1]} stateFilter={query.get('state') ?? undefined} />
      {:else if section === 'needs'}
        <NeedsPage />
      {:else if section === 'inbox'}
        <InboxPage tab={query.get('tab') === 'sent' ? 'sent' : 'inbox'} />
      {:else if section === 'thread'}
        <ThreadPage id={Number(parts[1])} focus={query.get('m') ? Number(query.get('m')) : undefined} />
      {:else if section === 'archive'}
        <ArchivePage {query} />
      {:else if section === 'compose'}
        <ComposePage {query} />
      {:else if section === 'admin'}
        {#if parts[1] === 'lists'}<AdminLists />
        {:else if parts[1] === 'links'}<AdminLinks />
        {:else if parts[1] === 'tokens'}<AdminTokens />
        {:else if parts[1] === 'settings'}<AdminSettings />
        {:else}<AdminSeats />{/if}
      {:else}
        <OrgPage selected={undefined} stateFilter={undefined} />
      {/if}
    </main>
  </div>
  {#if ui.helpOpen}<HelpOverlay />{/if}
{/if}

<style>
  .shell {
    display: grid;
    grid-template-areas: 'top top' 'nav main';
    grid-template-columns: 172px 1fr;
    grid-template-rows: auto 1fr;
    height: 100%;
  }
  .main {
    grid-area: main;
    overflow: auto;
    min-width: 0;
    min-height: 0;
    position: relative;
  }
  .banner {
    padding: 6px 14px;
    background: var(--danger-weak);
    display: flex;
    gap: 10px;
    align-items: center;
  }
  @media (max-width: 720px) {
    .shell {
      grid-template-areas: 'top' 'nav' 'main';
      grid-template-columns: 1fr;
      grid-template-rows: auto auto 1fr;
    }
  }
</style>
