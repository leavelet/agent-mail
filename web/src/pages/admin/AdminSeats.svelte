<script lang="ts">
  import Vis from '../../components/Vis.svelte';
  import { visible } from '../../lib/visible';
  import { get, post, patch, del, enc } from '../../lib/api';
  import { store } from '../../lib/store.svelte';
  import { t } from '../../lib/ui.svelte';
  import { href } from '../../lib/router.svelte';
  import type { PermissionMode, Seat } from '../../lib/types';

  interface Form {
    name: string;
    parent: string;
    role: string;
    harness: string;
    model: string;
    permission_mode: '' | PermissionMode;
    group: string;
  }
  const blank = (): Form => ({ name: '', parent: '', role: '', harness: '', model: '', permission_mode: '', group: '' });

  let form = $state<Form>(blank());
  let editing = $state<string | null>(null);
  let edit = $state<Form>(blank());
  let filter = $state('');
  let msg = $state<string | null>(null);
  let error = $state<string | null>(null);

  const seats = $derived(store.seats.filter((s) => !filter || s.name.includes(filter.toLowerCase())));
  const nul = (s: string) => (s.trim() === '' ? null : s.trim());

  async function act(f: () => Promise<unknown>) {
    error = null;
    msg = null;
    try {
      await f();
      await store.refreshSeats();
      msg = t('admin.saved');
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function create(e: SubmitEvent) {
    e.preventDefault();
    void act(async () => {
      const body: Record<string, string> = { name: form.name.trim() };
      for (const k of ['parent', 'role', 'harness', 'model', 'permission_mode', 'group'] as const) {
        const v = nul(form[k]);
        if (v) body[k] = v;
      }
      await post('/v1/seats', body);
      form = blank();
    });
  }

  async function startEdit(s: Seat) {
    // The seat list carries only the start of a long role.
    if (s.role_truncated) {
      try {
        s = await get<Seat>(`/v1/seats/${enc(s.name)}`);
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
        return;
      }
    }
    editing = s.name;
    edit = {
      name: s.name,
      parent: s.parent ?? '',
      role: s.role ?? '',
      harness: s.harness ?? '',
      model: s.model ?? '',
      permission_mode: s.permission_mode ?? '',
      group: s.group ?? '',
    };
  }

  function save(e: SubmitEvent) {
    e.preventDefault();
    const name = editing;
    if (!name) return;
    void act(async () => {
      await patch(`/v1/seats/${enc(name)}`, {
        parent: nul(edit.parent),
        role: nul(edit.role),
        harness: nul(edit.harness),
        model: nul(edit.model),
        permission_mode: nul(edit.permission_mode),
        group: nul(edit.group),
      });
      editing = null;
    });
  }

  function remove(name: string) {
    if (!confirm(t('admin.confirmDelete', { name }))) return;
    void act(() => del(`/v1/seats/${enc(name)}`));
  }
</script>

<div class="page">
  <div class="page-head"><h1>{t('nav.admin')} · {t('nav.seats')}</h1><span class="faint">{t('admin.seats.count', { n: store.seats.length })}</span></div>
  <form class="card create" onsubmit={create}>
    <label>{t('admin.name')}<input class="mono" required bind:value={form.name} size="14" /></label>
    <label>{t('admin.parent')}<input class="mono" list="seat-names" bind:value={form.parent} size="12" /></label>
    <label>{t('admin.harness')}
      <select bind:value={form.harness}><option value="">—</option><option>claude</option><option>codex</option></select></label>
    <label>{t('admin.model')}<input bind:value={form.model} size="12" /></label>
    <label>{t('admin.permission')}
      <select bind:value={form.permission_mode}><option value="">—</option><option>default</option><option>unattended</option></select></label>
    <label>{t('admin.group')}<input class="mono" bind:value={form.group} size="10" /></label>
    <label class="wide">{t('admin.role')}<textarea rows="2" bind:value={form.role}></textarea></label>
    <button class="primary" type="submit">{t('admin.create')}</button>
  </form>
  <datalist id="seat-names">{#each store.seats as s}<option value={s.name}></option>{/each}</datalist>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if msg}<p class="ok" role="status">{msg}</p>{/if}
  <div class="row"><input type="search" data-search placeholder={t('org.filter')} aria-label={t('org.filter')} bind:value={filter} /></div>
  <table class="card">
    <thead>
      <tr><th>{t('admin.name')}</th><th>{t('admin.kind')}</th><th>{t('admin.parent')}</th><th>{t('admin.harness')} · {t('admin.model')}</th><th>{t('admin.permission')}</th><th>{t('admin.group')}</th><th>{t('admin.role')}</th><th></th></tr>
    </thead>
    <tbody>
      {#each seats as s (s.name)}
        {#if editing === s.name}
          <tr class="editing">
            <td colspan="8">
              <form class="create inline" onsubmit={save}>
                <strong class="mono">{s.name}</strong>
                <label>{t('admin.parent')}<input class="mono" list="seat-names" bind:value={edit.parent} size="12" /></label>
                <label>{t('admin.harness')}<select bind:value={edit.harness}><option value="">—</option><option>claude</option><option>codex</option></select></label>
                <label>{t('admin.model')}<input bind:value={edit.model} size="12" /></label>
                <label>{t('admin.permission')}<select bind:value={edit.permission_mode}><option value="">—</option><option>default</option><option>unattended</option></select></label>
                <label>{t('admin.group')}<input class="mono" bind:value={edit.group} size="10" /></label>
                <label class="wide">{t('admin.role')}<textarea rows="3" bind:value={edit.role}></textarea></label>
                <button class="primary" type="submit">{t('admin.save')}</button>
                <button type="button" onclick={() => (editing = null)}>{t('admin.cancel')}</button>
              </form>
            </td>
          </tr>
        {:else}
          <tr>
            <td class="mono"><a href={href(['org', s.name])}>{s.name}</a></td>
            <td class="faint">{s.kind}</td>
            <td class="mono">{s.parent ?? ''}</td>
            <td><Vis text={[s.harness, s.model].filter(Boolean).join(' · ')} /></td>
            <td class="faint">{s.permission_mode ?? ''}</td>
            <td class="mono faint">{s.group ?? ''}</td>
            <td class="role" title={visible(s.role ?? '')}><Vis text={s.role?.split('\n', 1)[0] ?? ''} /></td>
            <td class="acts">
              {#if s.kind === 'agent'}
                <button class="small" onclick={() => startEdit(s)}>{t('admin.edit')}</button>
                <button class="small danger" onclick={() => remove(s.name)}>{t('admin.delete')}</button>
              {/if}
            </td>
          </tr>
        {/if}
      {/each}
    </tbody>
  </table>
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
  .create.inline {
    padding: 4px 0;
    margin: 0;
    border: none;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  label.wide {
    flex-basis: 100%;
  }
  label.wide textarea {
    width: 100%;
  }
  table {
    margin-top: 8px;
  }
  .role {
    max-width: 320px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--muted);
  }
  .acts {
    white-space: nowrap;
    text-align: right;
  }
  .row input {
    width: 240px;
  }
</style>
