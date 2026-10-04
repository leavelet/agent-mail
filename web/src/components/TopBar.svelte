<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { SEAT_STATES } from '../lib/types';
  import LangTheme from './LangTheme.svelte';

  const ov = $derived(store.overview);
  const needs = $derived(store.attention.length);
</script>

<header class="top">
  <a class="brand" href={href(['org'])}><span class="logo" aria-hidden="true">✉</span>amail</a>
  <span class="org mono" title={t('admin.org')}>{store.me?.org}</span>
  <div class="summary" aria-label={t('org.title')}>
    {#if ov}
      {#each SEAT_STATES as s}
        <a class="stat st-{s}" href={href(['org'], { state: s })} title={t(`state.${s}.long`)}>
          <span class="dot" aria-hidden="true"></span><span class="num">{ov.states[s] ?? 0}</span>
          <span class="lbl">{t(`state.${s}`)}</span>
        </a>
      {/each}
      {#if ov.turn_failed}
        <a class="stat failing" href={href(['org'], { state: 'needs' })} title={t('top.turnFailed.long')} data-testid="top-turn-failed">
          <span class="num">⚠ {ov.turn_failed}</span><span class="lbl">{t('top.turnFailed')}</span>
        </a>
      {/if}
      <span class="sep" aria-hidden="true"></span>
      <a class="stat" href={href(['archive'])} title={t('archive.timeline')}>
        <span class="num">{t('top.lastHour', { n: ov.mail_last_hour })}</span>
      </a>
    {/if}
  </div>
  <span class="spacer"></span>
  <a class="needs" class:hot={needs > 0} href={href(['needs'])}>{t('top.needsYou', { n: needs })}</a>
  {#if !store.live}<span class="live-off" title={t('app.offline')}>●</span>{/if}
  <LangTheme />
  <button class="ghost small" title={t('top.help')} aria-label={t('top.help')} onclick={() => (ui.helpOpen = true)}>?</button>
  <button class="small" onclick={() => store.logout()}>{t('top.logout')}</button>
</header>

<style>
  .top {
    grid-area: top;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 12px;
    height: 42px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    min-width: 0;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 7px;
    font-weight: 700;
    color: var(--text);
    font-size: 14px;
  }
  .brand:hover {
    text-decoration: none;
  }
  .logo {
    width: 22px;
    height: 22px;
    border-radius: 5px;
    background: var(--accent);
    color: var(--accent-text);
    display: grid;
    place-items: center;
    font-size: 13px;
  }
  .org {
    color: var(--muted);
    padding: 1px 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
  }
  .summary {
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
    overflow: hidden;
  }
  .stat {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 7px;
    border-radius: 5px;
    color: var(--text);
    white-space: nowrap;
    font-size: 12px;
  }
  .stat:hover {
    background: var(--surface-2);
    text-decoration: none;
  }
  .stat .lbl {
    color: var(--muted);
  }
  .stat .num {
    font-weight: 600;
  }
  .stat.failing {
    background: var(--danger-weak);
  }
  .stat.failing,
  .stat.failing .lbl {
    color: var(--danger);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--c, var(--faint));
  }
  .st-busy {
    --c: var(--st-busy);
  }
  .st-idle {
    --c: var(--st-idle);
  }
  .st-awaiting {
    --c: var(--st-awaiting);
  }
  .st-dormant {
    --c: var(--st-dormant);
  }
  .st-offline {
    --c: var(--st-offline);
  }
  .sep {
    width: 1px;
    height: 16px;
    background: var(--border);
    margin: 0 4px;
  }
  .needs {
    padding: 2px 10px;
    border-radius: 12px;
    background: var(--surface-2);
    color: var(--muted);
    font-weight: 600;
    white-space: nowrap;
  }
  .needs.hot {
    background: var(--danger);
    color: #fff;
  }
  .needs:hover {
    text-decoration: none;
  }
  .live-off {
    color: var(--warn);
  }
  @media (max-width: 900px) {
    .stat .lbl {
      display: none;
    }
  }
</style>
