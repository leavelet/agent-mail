<script lang="ts">
  import Vis from '../../components/Vis.svelte';
  import { get, post, del, enc } from '../../lib/api';
  import { t, ui } from '../../lib/ui.svelte';
  import { fullTime } from '../../lib/time';
  import type { TokenInfo, TokenResponse } from '../../lib/types';

  let tokens = $state.raw<TokenInfo[]>([]);
  let label = $state('');
  let fresh = $state.raw<TokenResponse | null>(null);
  let error = $state<string | null>(null);

  async function load() {
    try {
      tokens = await get<TokenInfo[]>('/v1/tokens');
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }
  $effect(() => {
    void load();
  });

  async function issue(scope: 'user' | 'node') {
    error = null;
    try {
      fresh = await post<TokenResponse>('/v1/tokens', { scope, label: label.trim() || undefined });
      label = '';
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function revoke(id: number) {
    if (!confirm(t('admin.confirmDelete', { name: `#${id}` }))) return;
    error = null;
    try {
      await del(`/v1/tokens/${enc(String(id))}`);
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }
</script>

<div class="page">
  <div class="page-head"><h1>{t('nav.admin')} · {t('nav.tokens')}</h1></div>
  <p class="muted">{t('admin.tokens.intro')}</p>
  <div class="row">
    <input placeholder={t('admin.label')} aria-label={t('admin.label')} bind:value={label} />
    <button onclick={() => issue('user')}>{t('admin.issueUser')}</button>
    <button onclick={() => issue('node')}>{t('admin.issueNode')}</button>
  </div>
  {#if fresh}
    <div class="fresh card" role="status">
      <p>{t('admin.newToken', { scope: fresh.scope })}</p>
      <code class="tok">{fresh.token}</code>
      <div class="row"><button class="small" onclick={() => navigator.clipboard?.writeText(fresh?.token ?? '')}>{t('seat.copy')}</button>
        <button class="small ghost" onclick={() => (fresh = null)}>{t('keys.close')}</button></div>
    </div>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <table class="card">
    <thead><tr><th>#</th><th>{t('admin.scope')}</th><th>{t('admin.seat')}</th><th>{t('admin.label')}</th><th>{t('admin.created')}</th><th></th></tr></thead>
    <tbody>
      {#each tokens as tk (tk.id)}
        <tr>
          <td class="num faint">{tk.id}</td>
          <td><span class="chip neutral">{tk.scope}</span></td>
          <td class="mono">{tk.seat ?? ''}</td>
          <td><Vis text={tk.label ?? ''} /></td>
          <td class="faint">{fullTime(tk.created_at, ui.lang)}</td>
          <td class="acts"><button class="small danger" onclick={() => revoke(tk.id)}>{t('admin.revoke')}</button></td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .row {
    margin: 8px 0 12px;
  }
  .fresh {
    padding: 10px 12px;
    margin-bottom: 12px;
    border-color: var(--warn);
    background: var(--warn-weak);
  }
  .fresh p {
    margin: 0 0 6px;
  }
  .tok {
    display: block;
    user-select: all;
    word-break: break-all;
    padding: 6px 8px;
    background: var(--surface);
    border-radius: 4px;
  }
  .acts {
    text-align: right;
  }
</style>
