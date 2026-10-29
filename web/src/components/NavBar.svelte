<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { href, router } from '../lib/router.svelte';
  import { navSection, sectionOf } from '../lib/nav';
  import LangTheme from './LangTheme.svelte';

  const route = $derived(sectionOf(router.route));
  // A thread belongs to no section of its own: it stays under the list it
  // was opened from; opened by its URL or after sending, under Sent or
  // Inbox as its messages say (none when they do not).
  let from = $state<string | null>(null);
  $effect(() => {
    if (route !== 'thread') from = route;
  });
  const guess = $derived(ui.thread && String(ui.thread.id) === router.route.parts[1] ? ui.thread.folder : null);
  const section = $derived(navSection(route, from, guess));
  const sub = $derived(router.route.parts[1] ?? 'seats');
  const invited = $derived(store.links.filter((l) => l.state === 'invited').length);
  const unread = $derived(store.me?.seat ? (store.counts.get(store.me.seat)?.unread ?? 0) : 0);
  const admin = ['seats', 'lists', 'openings', 'tokens', 'settings'] as const;
  // The phone menu closes when the page changes.
  $effect(() => {
    void router.route;
    ui.navOpen = false;
  });
</script>

<nav class="nav" id="main-nav" aria-label="main" class:open={ui.navOpen}>
  <a href={href(['needs'])} class:active={section === 'needs'}>
    <span>{t('nav.needs')}</span>
    {#if store.needsCount}<span class="count alert">{store.needsCount}</span>{/if}
  </a>
  <a href={href(['requests'], { state: 'all' })} class="sub" class:active={section === 'requests'}><span>{t('nav.requests')}</span></a>
  <a href={href(['org'])} class:active={section === 'org' || section === 'seat'}><span>{t('nav.org')}</span></a>
  <a href={href(['tasks'])} class:active={section === 'tasks'}><span>{t('nav.tasks')}</span></a>
  <a href={href(['docs'])} class:active={section === 'docs'}><span>{t('nav.docs')}</span></a>
  <a href={href(['teams'])} class:active={section === 'teams'}><span>{t('nav.teams')}</span></a>
  <a href={href(['orgs'])} class:active={section === 'orgs' || (section === 'admin' && sub === 'links')}>
    <span>{t('nav.others')}</span>
    {#if invited}<span class="count alert" title={t('others.invited', { n: invited })}>{invited}</span>{/if}
  </a>
  <a href={href(['inbox'])} class:active={section === 'inbox'}>
    <span>{t('nav.inbox')}</span>
    {#if unread}<span class="count hot">{unread}</span>{/if}
  </a>
  <a href={href(['inbox'], { tab: 'sent' })} class:active={section === 'sent'}><span>{t('nav.sent')}</span></a>
  <a href={href(['archive'])} class:active={section === 'archive'}><span>{t('nav.archive')}</span></a>
  <a href={href(['compose'])} class:active={section === 'compose'} class="compose"><span>{t('nav.compose')}</span><kbd>c</kbd></a>
  <div class="group">{t('nav.admin')}</div>
  {#each admin as a}
    <a href={href(['admin', a])} class="sub" class:active={section === 'admin' && sub === a}><span>{t(`nav.${a}`)}</span></a>
  {/each}
  <div class="phone">
    {#if store.me?.org}<span class="org mono">{store.me.org}</span>{/if}
    <LangTheme />
    <div class="row">
      <button class="ghost small" onclick={() => ((ui.navOpen = false), (ui.helpOpen = true))}>{t('top.help')}</button>
      <button class="small" onclick={() => store.logout()}>{t('top.logout')}</button>
    </div>
  </div>
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
  .phone {
    display: none;
  }
  /* Phone width: a menu over the page, opened from the top bar. */
  @media (max-width: 720px) {
    .nav {
      display: none;
      position: fixed;
      top: 42px;
      left: 0;
      right: 0;
      bottom: 0;
      z-index: 40;
      border-right: none;
      border-top: 1px solid var(--border);
      padding: 8px 12px 24px;
    }
    .nav.open {
      display: flex;
    }
    a {
      padding: 9px 10px;
    }
    .phone {
      display: flex;
      flex-direction: column;
      gap: 10px;
      margin: 16px 9px 0;
    }
    .phone .row {
      display: flex;
      gap: 8px;
    }
    .org {
      color: var(--muted);
    }
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
