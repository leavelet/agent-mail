<script lang="ts">
  import { post, del, enc } from '../../lib/api';
  import { store } from '../../lib/store.svelte';
  import { t } from '../../lib/ui.svelte';

  let org = $state('');
  let error = $state<string | null>(null);

  async function act(f: () => Promise<unknown>) {
    error = null;
    try {
      await f();
      await store.refreshDirectory();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }
</script>

<div class="page">
  <div class="page-head"><h1>{t('nav.admin')} · {t('nav.links')}</h1></div>
  <p class="muted">{t('admin.links.intro')}</p>
  <form class="row" onsubmit={(e) => { e.preventDefault(); void act(async () => { await post('/v1/links', { org: org.trim() }); org = ''; }); }}>
    <input class="mono" placeholder={t('admin.org')} aria-label={t('admin.org')} required bind:value={org} />
    <button class="primary" type="submit">{t('admin.requestLink')}</button>
  </form>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <table class="card">
    <thead><tr><th>{t('admin.org')}</th><th>{t('admin.state')}</th><th></th></tr></thead>
    <tbody>
      {#if store.links.length === 0}<tr><td colspan="3" class="faint">{t('admin.none')}</td></tr>{/if}
      {#each store.links as l (l.org)}
        <tr>
          <td class="mono">{l.org}</td>
          <td><span class="chip {l.state === 'active' ? 'ok' : l.state === 'invited' ? 'ack' : 'neutral'}">{t(`admin.link.${l.state}`)}</span></td>
          <td class="acts">
            {#if l.state === 'invited'}<button class="small primary" onclick={() => act(() => post(`/v1/links/${enc(l.org)}/accept`))}>{t('admin.accept')}</button>{/if}
            <button class="small danger" onclick={() => confirm(t('admin.confirmDelete', { name: l.org })) && act(() => del(`/v1/links/${enc(l.org)}`))}>{t('admin.revoke')}</button>
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  form {
    margin: 8px 0 12px;
  }
  .acts {
    text-align: right;
    white-space: nowrap;
  }
</style>
