<script lang="ts">
  // A seat's handover note (`GET /v1/seats/{name}/handover`): the current
  // note with its fields, older revisions, whether a session read it, an
  // open request; the user's request button and an editor to write the
  // note in the seat's name.
  import Vis from './Vis.svelte';
  import { get, post, put, qs, enc } from '../lib/api';
  import { t, ui } from '../lib/ui.svelte';
  import { fullTime, shortTime } from '../lib/time';
  import { HANDOVER_FIELDS, HANDOVER_LABELS, handoverTemplate } from '../lib/docs';
  import { docError } from '../lib/docops';
  import type { HandoverRequested, HandoverResponse } from '../lib/types';

  let { seat }: { seat: string } = $props();

  let data = $state.raw<HandoverResponse | null>(null);
  let rev = $state<number | null>(null);
  let error = $state<string | null>(null);
  let note = $state<string | null>(null);
  let busy = $state(false);
  let editing = $state(false);
  let draft = $state('');
  let draftPrivate = $state(false);
  let gen = 0;

  const path = $derived(`/v1/seats/${enc(seat)}/handover`);

  async function load() {
    const g = ++gen;
    try {
      const r = await get<HandoverResponse>(`${path}${qs({ rev: rev ?? undefined })}`);
      if (g !== gen) return;
      data = r;
      error = null;
    } catch (e) {
      if (g === gen) {
        data = null;
        error = docError(e);
      }
    }
  }

  $effect(() => {
    void seat;
    void rev;
    void load();
  });
  $effect(() => {
    void seat;
    rev = null;
    editing = false;
    note = null;
  });

  async function request() {
    busy = true;
    error = null;
    note = null;
    try {
      const r = await post<HandoverRequested>(`${path}/request`);
      note = t('handover.requested', { time: shortTime(r.requested_at, ui.now, ui.lang) });
      await load();
    } catch (e) {
      error = docError(e);
    } finally {
      busy = false;
    }
  }

  function startEdit() {
    draft = data?.note && data.note.rev === data.current_rev ? noteAsHeadings() : handoverTemplate();
    draftPrivate = !!data?.private;
    editing = true;
  }

  /** The current note with each field under its heading (the form the
   * server reads back). */
  function noteAsHeadings(): string {
    const f = data?.note?.fields;
    if (!f) return handoverTemplate();
    return HANDOVER_FIELDS.map((k) => `## ${HANDOVER_LABELS[k]}\n${f[k] ?? ''}\n`).join('\n');
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    note = null;
    try {
      data = await put<HandoverResponse>(path, { text: draft, private: draftPrivate });
      rev = null;
      editing = false;
      note = t('handover.saved', { rev: data.current_rev });
    } catch (err) {
      error = docError(err);
    } finally {
      busy = false;
    }
  }

  const n = $derived(data?.note);
  const isCurrent = $derived(!!n && n.rev === data?.current_rev);
</script>

<div class="handover" data-testid="seat-handover">
  <div class="row wrap head">
    {#if data && data.revisions.length > 1}
      <label class="row">{t('handover.revision')}
        <select value={rev ?? ''} onchange={(e) => { const v = (e.currentTarget as HTMLSelectElement).value; rev = v ? Number(v) : null; }} aria-label={t('handover.revision')} data-testid="handover-rev">
          <option value="">{t('handover.current', { rev: data.current_rev })}</option>
          {#each data.revisions.filter((r) => r.rev !== data?.current_rev) as r (r.rev)}
            <option value={r.rev}>{t('handover.revOption', { rev: r.rev, time: shortTime(r.written_at, ui.now, ui.lang), source: t(`handover.source.${r.source}`) })}</option>
          {/each}
        </select>
      </label>
    {/if}
    <span class="spacer"></span>
    <button class="small" disabled={busy} onclick={request} data-testid="handover-request">{t('handover.request')}</button>
    {#if !editing}<button class="small" onclick={startEdit} data-testid="handover-edit">{t('handover.write')}</button>{/if}
    <button class="small ghost" onclick={() => load()}>{t('handover.reload')}</button>
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if note}<p class="ok" role="status">{note}</p>{/if}
  {#if data?.requested_at}
    <p class="req" data-testid="handover-requested">{t('handover.openRequest', { seat: data.requested_by ?? '—', time: shortTime(data.requested_at, ui.now, ui.lang) })}</p>
  {/if}

  {#if editing}
    <form class="card edit" onsubmit={save}>
      <p class="faint hint">{t('handover.editHint')}</p>
      <textarea class="mono" rows="18" aria-label={t('handover.write')} bind:value={draft} data-testid="handover-text"></textarea>
      <div class="row wrap">
        <label class="check"><input type="checkbox" bind:checked={draftPrivate} />{t('handover.private')}</label>
        <span class="faint">{t('handover.chars', { n: draft.length, max: 16384 })}</span>
        <span class="spacer"></span>
        <button class="primary small" type="submit" disabled={busy}>{t('handover.save')}</button>
        <button class="small" type="button" onclick={() => (editing = false)}>{t('admin.cancel')}</button>
      </div>
    </form>
  {/if}

  {#if !data && !error}
    <p class="faint">{t('app.loading')}</p>
  {:else if data && !n}
    <p class="empty">{t('handover.none')}</p>
  {:else if n}
    <div class="meta row wrap" data-testid="handover-meta">
      <span class="chip {isCurrent ? 'ok' : 'neutral'}">{isCurrent ? t('handover.isCurrent') : t('handover.old')}</span>
      <span class="mono">{t('doc.revN', { rev: n.rev })}</span>
      <span title={fullTime(n.written_at, ui.lang)}>{t('handover.writtenAt', { time: shortTime(n.written_at, ui.now, ui.lang) })}</span>
      <span class="chip neutral" title={t(`handover.source.${n.source}.long`)}>{t(`handover.source.${n.source}`)}</span>
      {#if n.draft}<span class="chip attn">{t('handover.draft')}</span>{/if}
      {#if n.written_by}<span class="chip neutral">{t('mail.writtenBy', { user: n.written_by })}</span>{/if}
      {#if n.private || data?.private}<span class="chip ack">{t('handover.privateChip')}</span>{/if}
      {#if n.pointers_filled}<span class="faint">{t('handover.pointersFilled')}</span>{/if}
      <span class="faint">{t('handover.chars', { n: n.chars, max: 16384 })}</span>
      {#if n.session}<span class="faint mono" title={n.session}>{t('handover.session', { id: n.session.slice(0, 12) })}</span>{/if}
    </div>
    <p class="read" data-testid="handover-read">
      {#if n.read_at}
        {t('handover.readBy', { session: (n.read_by_session ?? '—').slice(0, 12), time: shortTime(n.read_at, ui.now, ui.lang) })}
      {:else}
        <span class="warn">{t('handover.notRead')}</span>
      {/if}
    </p>
    <dl class="fields card">
      {#each HANDOVER_FIELDS as f}
        <dt>{t(`handover.field.${f}`)}</dt>
        <dd data-field={f}>{#if n.fields[f]}<pre><Vis text={n.fields[f]} /></pre>{:else}<span class="faint">—</span>{/if}</dd>
      {/each}
    </dl>
    {#if data && data.revisions.length}
      <details class="revs">
        <summary>{t('handover.revisions', { n: data.revisions.length })}</summary>
        <table>
          <tbody>
            {#each data.revisions as r (r.rev)}
              <tr>
                <td class="mono num">{r.rev}</td>
                <td title={fullTime(r.written_at, ui.lang)}>{shortTime(r.written_at, ui.now, ui.lang)}</td>
                <td>{t(`handover.source.${r.source}`)}{#if r.written_by} · {t('mail.writtenBy', { user: r.written_by })}{/if}</td>
                <td class="num">{r.chars}</td>
                <td>{#if r.read_at}{t('handover.readShort', { time: shortTime(r.read_at, ui.now, ui.lang) })}{:else}<span class="faint">{t('handover.notReadShort')}</span>{/if}</td>
                <td><button class="small ghost" onclick={() => (rev = r.rev === data?.current_rev ? null : r.rev)}>{t('doc.revShow')}</button></td>
              </tr>
            {/each}
          </tbody>
        </table>
      </details>
    {/if}
  {/if}
</div>

<style>
  .handover {
    display: flex;
    flex-direction: column;
    gap: 8px;
    overflow: auto;
    min-height: 0;
    padding: 6px 0;
  }
  .head label {
    font-size: 12px;
    color: var(--muted);
    gap: 4px;
  }
  .req {
    margin: 0;
    padding: 4px 10px;
    border-radius: var(--radius);
    background: var(--notice-weak);
    color: var(--notice);
  }
  .meta {
    font-size: 12px;
  }
  .read {
    margin: 0;
    font-size: 12px;
  }
  .warn {
    color: var(--warn);
  }
  .fields {
    margin: 0;
    padding: 8px 12px;
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 6px 14px;
  }
  .fields dt {
    color: var(--muted);
    font-weight: 600;
  }
  .fields dd {
    margin: 0;
    min-width: 0;
  }
  .fields pre {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font);
  }
  .edit {
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .edit textarea {
    width: 100%;
    font-size: 12px;
  }
  .check {
    display: flex;
    gap: 4px;
    align-items: center;
    font-size: 12px;
  }
  .hint {
    margin: 0;
    font-size: 12px;
  }
  .revs summary {
    cursor: pointer;
    color: var(--muted);
  }
  .revs table {
    font-size: 12px;
  }
  @media (max-width: 720px) {
    .fields {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
