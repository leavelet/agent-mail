<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { href, router } from '../lib/router.svelte';

  const route = $derived(router.route.parts[0] ?? 'org');
  // A thread belongs to no section of its own: it stays under the list it
  // was opened from (none when it was opened directly or after sending).
  let from = $state<string | null>(null);
  $effect(() => {
    if (route !== 'thread') from = route;
  });
  const section = $derived(route === 'thread' ? (from === 'compose' ? null : from) : route);
  const sub = $derived(router.route.parts[1] ?? 'seats');
  const unread = $derived(store.me?.seat ? (store.counts.get(store.me.seat)?.unread ?? 0) : 0);
  const admin = ['seats', 'lists', 'links', 'tokens', 'settings'] as const;
</script>

<nav class="nav" aria-label="main">
  <a href={href(['needs'])} class:active={section === 'needs'}>
    <span>{t('nav.needs')}</span>
    {#if store.attention.length}<span class="count alert">{store.attention.length}</span>{/if}
  </a>
  <a href={href(['org'])} class:active={section === 'org' || section === 'seat'}><span>{t('nav.org')}</span></a>
  <a href={href(['inbox'])} class:active={section === 'inbox'}>
    <span>{t('nav.inbox')}</span>
    {#if unread}<span class="count hot">{unread}</span>{/if}
  </a>
  <a href={href(['archive'])} class:active={section === 'archive'}><span>{t('nav.archive')}</span></a>
  <a href={href(['compose'])} class:active={section === 'compose'} class="compose"><span>{t('nav.compose')}</span><kbd>c</kbd></a>
  <div class="group">{t('nav.admin')}</div>
  {#each admin as a}
    <a href={href(['admin', a])} class="sub" class:active={section === 'admin' && sub === a}><span>{t(`nav.${a}`)}</span></a>
  {/each}
</nav>

<style>
  .nav {
    grid-area: nav;
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 10px 8px;
    background: var(--surface);
    border-right: 1px solid var(--border);
    overflow-y: auto;
  }
  a {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
    padding: 5px 9px;
    border-radius: 6px;
    color: var(--text);
  }
  a:hover {
    background: var(--surface-2);
    text-decoration: none;
  }
  a.active {
    background: var(--accent-weak);
    color: var(--accent);
    font-weight: 600;
  }
  a.sub {
    padding-left: 16px;
    color: var(--muted);
  }
  a.sub.active {
    color: var(--accent);
  }
  .compose kbd {
    opacity: 0.7;
  }
  .group {
    margin: 12px 9px 3px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--faint);
    font-weight: 600;
  }
</style>
