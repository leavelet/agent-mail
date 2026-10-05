<script lang="ts">
  import Vis from '../../components/Vis.svelte';
  import { get, post, put, del, enc } from '../../lib/api';
  import { store } from '../../lib/store.svelte';
  import { t } from '../../lib/ui.svelte';
  import type { DeliveryMode, ListInfo, PostPolicy } from '../../lib/types';

  let name = $state('');
  let description = $state('');
  let policy = $state<PostPolicy>('anyone');
  let open = $state<string | null>(null);
  let detail = $state.raw<ListInfo | null>(null);
  let member = $state('');
  let mode = $state<DeliveryMode>('immediate');
  let error = $state<string | null>(null);

  async function act(f: () => Promise<unknown>) {
    error = null;
    try {
      await f();
      await store.refreshDirectory();
      if (open) detail = await get<ListInfo>(`/v1/lists/${enc(open)}`);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function show(n: string) {
    open = n;
    detail = null;
    try {
      detail = await get<ListInfo>(`/v1/lists/${enc(n)}`);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function create(e: SubmitEvent) {
    e.preventDefault();
    void act(async () => {
      await post('/v1/lists', { name: name.trim(), description: description.trim() || undefined, post_policy: policy });
      name = '';
      description = '';
    });
  }

  function addMember(e: SubmitEvent) {
    e.preventDefault();
    const l = open;
    if (!l || !member.trim()) return;
    void act(async () => {
      await put(`/v1/lists/${enc(l)}/members/${enc(member.trim())}`, { mode });
      member = '';
    });
  }
</script>

<div class="page">
  <div class="page-head"><h1>{t('nav.admin')} · {t('nav.lists')}</h1></div>
  <form class="card create" onsubmit={create}>
    <label>{t('admin.name')}<input class="mono" required bind:value={name} size="14" /></label>
    <label>{t('admin.description')}<input bind:value={description} size="30" /></label>
    <label>{t('admin.policy')}<select bind:value={policy}><option>anyone</option><option>members</option><option>user_only</option></select></label>
    <button class="primary" type="submit">{t('admin.create')}</button>
  </form>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <div class="cols">
    <table class="card">
      <thead><tr><th>{t('admin.name')}</th><th>{t('admin.description')}</th><th>{t('admin.policy')}</th><th>{t('admin.members')}</th><th></th></tr></thead>
      <tbody>
        {#each store.lists as l (l.name)}
          <tr class:sel={open === l.name}>
            <td class="mono"><button class="linkish" onclick={() => show(l.name)}>{l.name}</button>{#if l.managed} <span class="chip neutral">{t('admin.managed')}</span>{/if}</td>
            <td class="muted"><Vis text={l.description ?? ''} /></td>
            <td class="faint">{l.post_policy}</td>
            <td class="num">{l.member_count}</td>
            <td class="acts"><button class="small danger" onclick={() => confirm(t('admin.confirmDelete', { name: l.name })) && act(() => del(`/v1/lists/${enc(l.name)}`))}>{t('admin.delete')}</button></td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if open}
      <div class="card members">
        <h2 class="mono">{open}</h2>
        <form class="row wrap" onsubmit={addMember}>
          <input class="mono" placeholder={t('admin.member')} aria-label={t('admin.member')} list="addr-names" bind:value={member} />
          <select bind:value={mode} aria-label={t('admin.mode')}><option>immediate</option><option>digest</option><option>archive</option></select>
          <button type="submit">{t('admin.add')}</button>
        </form>
        <datalist id="addr-names">{#each store.seats as s}<option value={s.name}></option>{/each}</datalist>
        {#if detail?.members}
          <table>
            <tbody>
              {#each detail.members as m (m.address)}
                <tr><td class="mono">{m.address}{#if m.inactive} <span class="chip neutral" title={t('admin.inactive.long')}>{t('admin.inactive')}</span>{/if}</td><td class="faint">{m.mode}</td>
                  <td class="acts"><button class="small ghost danger" onclick={() => act(() => del(`/v1/lists/${enc(open ?? '')}/members/${enc(m.address)}`))}>{t('admin.delete')}</button></td></tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .create {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 12px;
    align-items: flex-end;
    padding: 10px 12px;
    margin-bottom: 10px;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  .cols {
    display: grid;
    grid-template-columns: 1fr minmax(280px, 360px);
    gap: 12px;
    align-items: start;
  }
  .members {
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .linkish {
    border: none;
    background: none;
    padding: 0;
    color: var(--accent);
    font-family: var(--mono);
  }
  tr.sel {
    background: var(--accent-weak);
  }
  .acts {
    text-align: right;
  }
</style>
