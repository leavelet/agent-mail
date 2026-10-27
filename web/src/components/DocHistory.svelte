<script lang="ts">
  // A document's revisions with a diff between any two of them, a past
  // revision's text, and the events (`GET /v1/docs/{num}/history`, loaded
  // in pages). `version` changes when the document changed: everything is
  // loaded again.
  import Vis from './Vis.svelte';
  import DocDiff from './DocDiff.svelte';
  import { get, qs } from '../lib/api';
  import { t, ui } from '../lib/ui.svelte';
  import { fullTime, shortTime } from '../lib/time';
  import { docError, docPath } from '../lib/docops';
  import { isKey } from '../lib/i18n';
  import type { DocDetail, DocDiff as Diff, DocEvent, DocHistory, DocRevision } from '../lib/types';

  let { num, version, rev }: { num: number; version: number; rev: number } = $props();

  let revisions = $state.raw<DocRevision[]>([]);
  let events = $state.raw<DocEvent[]>([]);
  let next = $state<number | undefined>(undefined);
  let error = $state<string | null>(null);
  let from = $state(0);
  let to = $state(0);
  let diff = $state.raw<Diff | null>(null);
  let diffError = $state<string | null>(null);
  let shown = $state.raw<{ rev: number; text: string } | null>(null);
  let gen = 0;

  async function load(more = false) {
    const g = ++gen;
    try {
      const after = more ? (events[events.length - 1]?.seq ?? 0) : 0;
      const r = await get<DocHistory>(`${docPath(num, '/history')}${qs({ after_seq: after || undefined, limit: 200 })}`);
      if (g !== gen) return;
      if (!more) revisions = [...r.revisions].sort((a, b) => b.rev - a.rev);
      events = more ? [...events, ...r.events] : r.events;
      next = r.next_after_seq;
      error = null;
    } catch (e) {
      if (g === gen) error = docError(e);
    }
  }

  $effect(() => {
    void num;
    void version;
    void load();
  });

  // The default comparison: the last change.
  $effect(() => {
    to = rev;
    from = Math.max(1, rev - 1);
  });

  async function showDiff() {
    diffError = null;
    try {
      diff = await get<Diff>(`${docPath(num, '/diff')}${qs({ from, to })}`);
    } catch (e) {
      diffError = docError(e);
    }
  }

  async function showRev(r: number) {
    if (shown?.rev === r) {
      shown = null;
      return;
    }
    try {
      const d = await get<DocDetail>(`${docPath(num)}${qs({ rev: r })}`);
      shown = { rev: r, text: d.read?.text ?? '' };
    } catch (e) {
      diffError = docError(e);
    }
  }

  function eventText(e: DocEvent): string {
    const k = `doc.ev.${e.kind}`;
    const base = isKey(k) ? t(k) : e.kind;
    const parts = [base];
    if (e.field) parts.push(e.field);
    if (e.from || e.to) parts.push(`${e.from ?? '—'} → ${e.to ?? '—'}`);
    if (e.rev) parts.push(t('doc.revN', { rev: e.rev }));
    return parts.join(' · ');
  }
</script>

<section class="hist" data-testid="doc-history">
  <h2>{t('doc.revisions')} <span class="count">{revisions.length}</span></h2>
  {#if error}<p class="error">{error}</p>{/if}
  {#if revisions.length > 1}
    <div class="row wrap cmp">
      <label>{t('doc.diff.from')}
        <select bind:value={from} aria-label={t('doc.diff.from')}>{#each revisions as r (r.rev)}<option value={r.rev}>{r.rev}</option>{/each}</select>
      </label>
      <label>{t('doc.diff.to')}
        <select bind:value={to} aria-label={t('doc.diff.to')}>{#each revisions as r (r.rev)}<option value={r.rev}>{r.rev}</option>{/each}</select>
      </label>
      <button class="small" onclick={showDiff} data-testid="doc-diff-show">{t('doc.diff.show')}</button>
      {#if diff}<button class="small ghost" onclick={() => (diff = null)}>{t('doc.diff.hide')}</button>{/if}
    </div>
  {/if}
  {#if diffError}<p class="error">{diffError}</p>{/if}
  {#if diff}
    <p class="faint">{t('doc.diff.title', { from: diff.from, to: diff.to })}</p>
    <DocDiff diff={diff.diff} />
  {/if}
  <table class="revs">
    <thead><tr><th>{t('doc.rev')}</th><th>{t('doc.author')}</th><th>{t('doc.revMessage')}</th><th>{t('doc.when')}</th><th></th></tr></thead>
    <tbody>
      {#each revisions as r (r.rev)}
        <tr data-rev={r.rev}>
          <td class="num mono">{r.rev}</td>
          <td class="mono">{r.author ?? '—'}{#if r.written_by}<span class="faint"> · {t('mail.writtenBy', { user: r.written_by })}</span>{/if}</td>
          <td>{#if r.message}<Vis text={r.message} />{:else}<span class="faint">—</span>{/if}</td>
          <td title={fullTime(r.at, ui.lang)}>{shortTime(r.at, ui.now, ui.lang)}</td>
          <td><button class="small ghost" onclick={() => showRev(r.rev)}>{shown?.rev === r.rev ? t('doc.diff.hide') : t('doc.revShow')}</button></td>
        </tr>
        {#if shown?.rev === r.rev}
          <tr><td colspan="5"><pre class="old"><Vis text={shown.text} /></pre></td></tr>
        {/if}
      {/each}
    </tbody>
  </table>

  <h2>{t('doc.history')} <span class="count">{events.length}</span></h2>
  <ol class="events">
    {#each events as e (e.seq)}
      <li>
        <span class="faint when" title={fullTime(e.at, ui.lang)}>{shortTime(e.at, ui.now, ui.lang)}</span>
        <span class="mono">{e.actor ?? t('doc.system')}</span>
        {#if e.written_by}<span class="faint">({t('mail.writtenBy', { user: e.written_by })})</span>{/if}
        <span><Vis text={eventText(e)} /></span>
        {#if e.note}<span class="faint"><Vis text={e.note} /></span>{/if}
      </li>
    {/each}
  </ol>
  {#if next}<button class="small" onclick={() => load(true)}>{t('doc.more')}</button>{/if}
</section>

<style>
  .hist {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .cmp label {
    display: flex;
    gap: 4px;
    align-items: center;
    font-size: 12px;
    color: var(--muted);
  }
  .revs {
    width: 100%;
    font-size: 12px;
  }
  .revs td {
    vertical-align: top;
  }
  .old {
    margin: 0;
    padding: 6px 10px;
    background: var(--surface-2);
    border-radius: var(--radius);
    max-height: 360px;
    overflow: auto;
    white-space: pre-wrap;
  }
  .events {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 12px;
  }
  .events li {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .when {
    min-width: 92px;
  }
</style>
