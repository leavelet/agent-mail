<script lang="ts">
  import { del, enc, get, post, request } from '../lib/api';
  import { t, ui } from '../lib/ui.svelte';
  import { fullTime, shortTime } from '../lib/time';
  import type { OperatorStats, OrgStat } from '../lib/types';
  import LangTheme from '../components/LangTheme.svelte';

  let stats = $state.raw<OperatorStats | null>(null);
  let error = $state<string | null>(null);
  let loadedAt = $state(0);
  let deleting = $state.raw<OrgStat | null>(null);
  let confirmName = $state('');

  async function load() {
    try {
      stats = await get<OperatorStats>('/v1/operator/stats');
      loadedAt = Date.now();
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  $effect(() => {
    void load();
    const id = setInterval(() => void load(), 10_000);
    return () => clearInterval(id);
  });

  async function act(o: OrgStat, op: 'suspend' | 'resume') {
    if (op === 'suspend' && !confirm(t('op.confirmSuspend', { org: o.name }))) return;
    try {
      await post(`/v1/operator/orgs/${enc(o.name)}/${op}`);
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function remove(e: SubmitEvent) {
    e.preventDefault();
    if (!deleting || confirmName !== deleting.name) return;
    try {
      await request('DELETE', `/v1/operator/orgs/${enc(deleting.name)}`, { confirm: confirmName });
      deleting = null;
      confirmName = '';
      await load();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  async function signOut() {
    try {
      await del('/v1/session');
    } catch {
      // Signed out either way.
    }
    location.href = '/';
  }
</script>

<div class="op" lang={ui.lang === 'zh' ? 'zh-CN' : 'en'}>
  <header class="top">
    <span class="brand"><span class="logo" aria-hidden="true">✉</span>amail · {t('op.title')}</span>
    <span class="spacer"></span>
    {#if loadedAt}<span class="faint small">{t('op.refreshed', { time: shortTime(new Date(loadedAt).toISOString(), ui.now, ui.lang) })}</span>{/if}
    <LangTheme />
    <button class="small" onclick={signOut}>{t('top.logout')}</button>
  </header>
  <main class="page">
    <p class="muted">{t('op.intro')}</p>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if stats}
      <div class="cards">
        <div class="card stat"><span class="num">{stats.orgs}</span><span>{t('op.orgs')}</span></div>
        <div class="card stat"><span class="num">{stats.suspended}</span><span>{t('op.suspended')}</span></div>
        <div class="card stat"><span class="num">{stats.seats}</span><span>{t('op.seats')}</span></div>
        <div class="card stat"><span class="num">{stats.nodes_online}</span><span>{t('op.nodes')}</span></div>
        <div class="card stat"><span class="num">{stats.mail_last_5min}</span><span>{t('op.mail5')}</span></div>
        <div class="card stat"><span class="num">{stats.mail_last_hour}</span><span>{t('op.mailHour')}</span></div>
      </div>
      {#if stats.list.length === 0}
        <p class="empty">{t('op.empty')}</p>
      {:else}
        <table class="card" data-testid="op-orgs">
          <thead>
            <tr>
              <th>{t('op.col.org')}</th><th class="num">{t('op.col.seats')}</th><th class="num">{t('op.col.nodes')}</th>
              <th class="num">{t('op.col.hour')}</th><th class="num">{t('op.col.day')}</th><th class="num">{t('op.col.total')}</th>
              <th>{t('op.col.last')}</th><th>{t('op.col.state')}</th><th></th>
            </tr>
          </thead>
          <tbody>
            {#each stats.list as o (o.name)}
              <tr data-org={o.name}>
                <td class="mono">{o.name}</td>
                <td class="num">{o.seats}</td>
                <td class="num">{o.nodes}</td>
                <td class="num">{o.messages_hour}</td>
                <td class="num">{o.messages_day}</td>
                <td class="num">{o.messages}</td>
                <td class="faint" title={o.last_activity ? fullTime(o.last_activity, ui.lang) : ''}>
                  {o.last_activity ? shortTime(o.last_activity, ui.now, ui.lang) : '–'}
                </td>
                <td>
                  {#if o.suspended_at}<span class="chip warn">{t('op.suspended')}</span>{:else}<span class="chip neutral">{t('op.active')}</span>{/if}
                </td>
                <td class="acts">
                  {#if o.suspended_at}
                    <button class="small" onclick={() => act(o, 'resume')}>{t('op.resume')}</button>
                  {:else}
                    <button class="small" onclick={() => act(o, 'suspend')}>{t('op.suspend')}</button>
                  {/if}
                  <button class="small danger" onclick={() => ((deleting = o), (confirmName = ''))}>{t('op.delete')}</button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    {/if}
  </main>
  {#if deleting}
    <div class="overlay" role="presentation" onclick={(e) => e.target === e.currentTarget && (deleting = null)}>
      <div class="card dialog" role="dialog" aria-modal="true" aria-labelledby="del-title">
      <form class="dform" onsubmit={remove}>
        <h2 id="del-title">{t('op.deleteTitle', { org: deleting.name })}</h2>
        <p>{t('op.deleteHint')}</p>
        <input aria-label={t('settings.deleteType')} placeholder={deleting.name} bind:value={confirmName} autocomplete="off" spellcheck="false" />
        <div class="row">
          <span class="spacer"></span>
          <button type="button" class="small" onclick={() => (deleting = null)}>{t('keys.close')}</button>
          <button type="submit" class="small danger" disabled={confirmName !== deleting.name}>{t('op.delete')}</button>
        </div>
      </form>
      </div>
    </div>
  {/if}
</div>

<style>
  .op {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 12px;
    height: 42px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 7px;
    font-weight: 700;
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
  .page {
    overflow: auto;
  }
  .cards {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    margin: 10px 0 14px;
  }
  .stat {
    padding: 10px 14px;
    min-width: 120px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    color: var(--muted);
  }
  .stat .num {
    font-size: 22px;
    font-weight: 700;
    color: var(--text);
  }
  td.num,
  th.num {
    text-align: right;
  }
  .acts {
    text-align: right;
    white-space: nowrap;
  }
  .small {
    font-size: 12px;
  }
  .overlay {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 35%);
    display: grid;
    place-items: center;
  }
  .dialog {
    width: min(460px, 92vw);
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .dform {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .dialog h2 {
    margin: 0;
    font-size: 16px;
  }
  .dialog p {
    margin: 0;
  }
</style>
