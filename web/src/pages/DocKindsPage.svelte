<script lang="ts">
  // The org's document kinds (`GET /v1/docs/kinds`) and the form that adds
  // a kind or changes one (`POST /v1/docs/kinds`, user only). A kind is
  // never deleted; a disabled kind takes no new documents.
  import { get, post } from '../lib/api';
  import { t } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { docError } from '../lib/docops';
  import type { DocKind } from '../lib/types';

  let kinds = $state.raw<DocKind[]>([]);
  let error = $state<string | null>(null);
  let note = $state<string | null>(null);
  let busy = $state(false);

  let name = $state('');
  let description = $state('');
  let sections = $state('');
  let activateBy = $state<'owner' | 'human'>('owner');
  let inIndex = $state(true);
  let capKib = $state('');
  let verifyDays = $state('');

  async function load() {
    try {
      kinds = (await get<{ kinds: DocKind[] }>('/v1/docs/kinds')).kinds;
      error = null;
    } catch (e) {
      error = docError(e);
    }
  }
  $effect(() => {
    void load();
  });

  /** Fill the form from a kind, to change it. */
  function edit(k: DocKind) {
    name = k.name;
    description = k.description ?? '';
    sections = k.required_sections.join(', ');
    activateBy = k.activate_by;
    inIndex = k.in_index;
    capKib = String(Math.round(k.body_cap_bytes / 1024));
    verifyDays = k.verify_days === undefined ? '' : String(k.verify_days);
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    note = null;
    try {
      const body: Record<string, unknown> = {
        name: name.trim(),
        description: description.trim() || undefined,
        required_sections: sections
          .split(',')
          .map((s) => s.trim())
          .filter(Boolean),
        activate_by: activateBy,
        in_index: inIndex,
      };
      if (capKib.trim()) body.body_cap_bytes = Math.round(Number(capKib) * 1024);
      if (verifyDays.trim()) body.verify_days = Number(verifyDays);
      await post('/v1/docs/kinds', body);
      note = t('kinds.saved', { name: name.trim() });
      name = '';
      description = '';
      sections = '';
      capKib = '';
      verifyDays = '';
      await load();
    } catch (err) {
      error = docError(err);
    } finally {
      busy = false;
    }
  }

  async function setDisabled(k: DocKind, disabled: boolean) {
    busy = true;
    error = null;
    try {
      await post('/v1/docs/kinds', { name: k.name, disabled });
      await load();
    } catch (err) {
      error = docError(err);
    } finally {
      busy = false;
    }
  }
</script>

<div class="page kindspage">
  <p class="back"><a href={href(['docs'])}>← {t('docs.title')}</a></p>
  <div class="page-head"><h1>{t('docs.kinds')}</h1></div>
  <p class="muted intro">{t('kinds.intro')}</p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if note}<p class="ok" role="status">{note}</p>{/if}
  <div class="scroll">
    <table data-testid="kinds-table">
      <thead>
        <tr><th>{t('kinds.name')}</th><th>{t('kinds.sections')}</th><th>{t('kinds.activateBy')}</th><th>{t('kinds.inIndex')}</th><th>{t('kinds.cap')}</th><th>{t('kinds.verifyDays')}</th><th>{t('kinds.docs')}</th><th></th></tr>
      </thead>
      <tbody>
        {#each kinds as k (k.name)}
          <tr data-kind={k.name} class:off={k.disabled}>
            <td>
              <a class="mono" href={href(['docs'], { kind: k.name })}>{k.name}</a>
              {#if k.builtin}<span class="chip neutral">{t('kinds.builtin')}</span>{/if}
              {#if k.disabled}<span class="chip attn">{t('kinds.disabled')}</span>{/if}
              {#if k.description}<div class="faint small">{k.description}</div>{/if}
            </td>
            <td>{k.required_sections.join(', ') || '—'}</td>
            <td>{t(`kinds.by.${k.activate_by}`)}</td>
            <td>{k.in_index ? t('kinds.yes') : t('kinds.no')}</td>
            <td class="num">{Math.round(k.body_cap_bytes / 1024)} KiB</td>
            <td class="num">{k.verify_days ?? '—'}</td>
            <td class="num">{k.docs}</td>
            <td class="acts">
              <button class="small ghost" onclick={() => edit(k)}>{t('admin.edit')}</button>
              <button class="small ghost" disabled={busy} onclick={() => setDisabled(k, !k.disabled)}>{k.disabled ? t('kinds.enable') : t('kinds.disable')}</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  <form class="card" onsubmit={save} data-testid="kind-form">
    <h2>{t('kinds.add')}</h2>
    <div class="row wrap">
      <label>{t('kinds.name')}<input class="mono" required maxlength="32" pattern="[a-z][a-z0-9_\-]*" bind:value={name} /></label>
      <label>{t('kinds.activateBy')}
        <select bind:value={activateBy}>
          <option value="owner">{t('kinds.by.owner')}</option>
          <option value="human">{t('kinds.by.human')}</option>
        </select>
      </label>
      <label>{t('kinds.cap')}<input class="short" inputmode="numeric" placeholder="512" bind:value={capKib} /></label>
      <label>{t('kinds.verifyDays')}<input class="short" inputmode="numeric" placeholder="30" bind:value={verifyDays} /></label>
      <label class="check"><input type="checkbox" bind:checked={inIndex} />{t('kinds.inIndex')}</label>
    </div>
    <label>{t('kinds.description')}<input maxlength="200" bind:value={description} /></label>
    <label>{t('kinds.sections')}<input placeholder="When, Steps, Verify" bind:value={sections} /></label>
    <div class="row">
      <button class="primary small" type="submit" disabled={busy || !name.trim()}>{t('admin.save')}</button>
      <span class="faint hint">{t('kinds.formHint')}</span>
    </div>
  </form>
</div>

<style>
  .kindspage {
    max-width: 1100px;
  }
  .back {
    margin: 0 0 6px;
    font-size: 12px;
  }
  .intro {
    font-size: 12px;
    max-width: 820px;
  }
  .scroll {
    overflow-x: auto;
    margin-bottom: 14px;
  }
  table {
    width: 100%;
  }
  tr.off {
    opacity: 0.6;
  }
  .small,
  .hint {
    font-size: 12px;
  }
  .acts {
    white-space: nowrap;
  }
  form {
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  label.check {
    flex-direction: row;
    align-items: center;
    gap: 4px;
  }
  .short {
    width: 80px;
  }
  @media (max-width: 720px) {
    .kindspage {
      padding: 12px 16px;
    }
  }
</style>
