<script lang="ts">
  import { get, qs } from '../lib/api';
  import { t } from '../lib/ui.svelte';
  import { go } from '../lib/router.svelte';
  import { fromHeader, fromHit, type MailRow } from '../lib/mailrows';
  import type { ArchiveResponse, SearchResponse } from '../lib/types';
  import MailList from '../components/MailList.svelte';

  let { query }: { query: URLSearchParams } = $props();

  const PAGE = 50;
  const SEARCH_REQUESTS = 10;
  // svelte-ignore state_referenced_locally
  let q = $state(query.get('q') ?? '');
  // svelte-ignore state_referenced_locally
  let from = $state(query.get('from') ?? '');
  // svelte-ignore state_referenced_locally
  let thread = $state(query.get('thread') ?? '');
  // svelte-ignore state_referenced_locally
  let since = $state(toLocal(query.get('since')));
  // svelte-ignore state_referenced_locally
  let until = $state(toLocal(query.get('until')));

  let rows = $state.raw<MailRow[]>([]);
  let next = $state<number | undefined>(undefined);
  let mode = $state<'index' | 'scan' | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);

  function toLocal(iso: string | null): string {
    if (!iso) return '';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return '';
    const pad = (n: number) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }
  function toIso(local: string): string | undefined {
    if (!local) return undefined;
    const d = new Date(local);
    return Number.isNaN(d.getTime()) ? undefined : d.toISOString();
  }

  const searching = $derived(!!query.get('q'));

  function url(beforeId?: number): string {
    const sq = query.get('q');
    if (sq) return `/v1/archive/search${qs({ q: sq, limit: PAGE, before_id: beforeId })}`;
    return `/v1/archive/messages${qs({
      from: query.get('from'),
      thread: query.get('thread'),
      since: query.get('since'),
      until: query.get('until'),
      limit: PAGE,
      before_id: beforeId,
    })}`;
  }

  async function load(beforeId?: number) {
    loading = true;
    error = null;
    try {
      if (query.get('q')) {
        // One search request looks at the org's mail for a limited time and
        // answers with a cursor; ask again until the page is full, for a
        // few requests at most.
        let got: MailRow[] = [];
        let cursor = beforeId;
        for (let i = 0; i < SEARCH_REQUESTS; i++) {
          const r = await get<SearchResponse>(url(cursor));
          got = [...got, ...r.messages.map(fromHit)];
          cursor = r.next_before_id;
          mode = r.mode;
          if (!cursor || got.length >= PAGE) break;
        }
        rows = beforeId ? [...rows, ...got] : got;
        next = cursor;
      } else {
        const r = await get<ArchiveResponse>(url(beforeId));
        const got = r.messages.map(fromHeader);
        rows = beforeId ? [...rows, ...got] : got;
        next = r.next_before_id;
        mode = null;
      }
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void query.toString();
    q = query.get('q') ?? '';
    rows = [];
    void load();
  });

  function search(e: SubmitEvent) {
    e.preventDefault();
    go(['archive'], { q: q.trim() || undefined });
  }
  function applyFilters(e: SubmitEvent) {
    e.preventDefault();
    go(['archive'], {
      from: from.trim() || undefined,
      thread: thread.trim() || undefined,
      since: toIso(since),
      until: toIso(until),
    });
  }
</script>

<div class="page archive">
  <div class="page-head"><h1>{t('archive.title')}</h1><span class="faint">{searching ? t('archive.results', { q: query.get('q') ?? '' }) : t('archive.timeline')}</span></div>
  <form class="searchbar row" onsubmit={search} role="search">
    <input type="search" data-search placeholder={t('archive.search')} aria-label={t('archive.search')} bind:value={q} />
    <button class="primary" type="submit">{t('archive.go')}</button>
    {#if searching}<button type="button" onclick={() => go(['archive'])}>{t('archive.clear')}</button>{/if}
  </form>
  {#if !searching}
    <form class="filters row wrap" onsubmit={applyFilters}>
      <label>{t('archive.from')} <input class="mono" bind:value={from} placeholder="seat" size="14" /></label>
      <label>{t('archive.thread')} <input class="mono" bind:value={thread} placeholder="#" size="6" inputmode="numeric" /></label>
      <label>{t('archive.since')} <input type="datetime-local" bind:value={since} /></label>
      <label>{t('archive.until')} <input type="datetime-local" bind:value={until} /></label>
      <button type="submit">{t('archive.apply')}</button>
    </form>
  {/if}
  {#if mode === 'scan' && next}
    <!-- Only while older mail is left to search: a search that read all of
         the org's mail has nothing to add. -->
    <p class="scan" role="note" data-mode="scan">ⓘ {t('archive.scanNote')}</p>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
  <div class="card results" data-mode={mode ?? 'timeline'}>
    {#if rows.length === 0 && !loading}
      <div class="empty">{t('archive.empty')}</div>
    {:else}
      <MailList {rows} label={t('archive.title')} onselect={(r) => go(['thread', r.thread_id], { m: r.id })} />
    {/if}
    {#if next}
      <div class="more"><button class="small" disabled={loading} onclick={() => load(next)}>{t('mail.older')}</button></div>
    {/if}
  </div>
</div>

<style>
  .archive {
    max-width: 1100px;
  }
  .searchbar input {
    flex: 1;
    max-width: 560px;
  }
  .filters {
    margin: 8px 0;
    gap: 10px;
  }
  .filters label {
    display: inline-flex;
    gap: 5px;
    align-items: center;
    color: var(--muted);
  }
  .scan {
    margin: 8px 0;
    padding: 6px 10px;
    border-radius: var(--radius);
    background: var(--warn-weak);
    color: var(--warn);
  }
  .results {
    margin-top: 10px;
    overflow: hidden;
  }
  .more {
    padding: 10px;
    text-align: center;
  }
</style>
