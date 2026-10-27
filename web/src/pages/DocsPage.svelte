<script lang="ts">
  // The org's documents. Without a kind or a query: the landing page of a
  // team (pinned documents, the kinds with counts, the stale and draft
  // counts, the newest documents). With `kind` (or another filter): the
  // list with its filters. With `q`: the section search. Everything is in
  // the URL (`#/docs?team=backend`, `#/docs?kind=runbook&stale=1`,
  // `#/docs?q=rollback`).
  import Vis from '../components/Vis.svelte';
  import DocChip from '../components/DocChip.svelte';
  import { get, qs } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { go, href } from '../lib/router.svelte';
  import { shortTime, fullTime } from '../lib/time';
  import { DOC_STATUSES, docHref, docId, docStatusTone, filterQuery, filtersFrom, summarize, type DocFilters } from '../lib/docs';
  import { docError, listDocs } from '../lib/docops';
  import type { Doc, DocKind, DocSearchResult } from '../lib/types';

  let { query }: { query: URLSearchParams } = $props();

  const team = $derived(query.get('team') ?? '');
  const q = $derived(query.get('q') ?? '');
  const filters = $derived(filtersFrom(query));
  const listing = $derived(!q && !!(filters.kind || filters.status || filters.owner || filters.label || filters.stale || filters.pinned));
  const mode = $derived(q ? 'search' : listing ? 'list' : 'landing');

  let kinds = $state.raw<DocKind[]>([]);
  $effect(() => {
    void store.docSeq;
    get<{ kinds: DocKind[] }>('/v1/docs/kinds')
      .then((r) => (kinds = r.kinds))
      .catch(() => undefined);
  });

  // The search box: the query in the URL.
  let searchDraft = $state('');
  let searchAll = $state(false);
  $effect(() => {
    searchDraft = q;
    searchAll = query.get('status') === 'all';
  });
  function search(e: SubmitEvent) {
    e.preventDefault();
    const s = searchDraft.trim();
    go(['docs'], s ? { q: s, team: team || undefined, status: searchAll ? 'all' : undefined } : { team: team || undefined });
  }

  // --- landing ---------------------------------------------------------------
  let teamDocs = $state.raw<Doc[]>([]);
  let teamMore = $state(false);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let gen = 0;

  async function loadLanding() {
    const g = ++gen;
    try {
      const r = await listDocs({ team: team || undefined, limit: 500 });
      if (g !== gen) return;
      teamDocs = r.docs;
      teamMore = !!r.next_before;
      error = null;
      loaded = true;
    } catch (e) {
      if (g === gen) error = docError(e);
    }
  }
  const summary = $derived(summarize(teamDocs));

  // --- list ------------------------------------------------------------------
  let docs = $state.raw<Doc[]>([]);
  let next = $state<number | undefined>(undefined);

  async function loadList(more = false) {
    const g = ++gen;
    try {
      const r = await listDocs({ ...filterQuery(filters), before: more ? next : undefined, limit: 100 });
      if (g !== gen) return;
      docs = more ? [...docs, ...r.docs] : r.docs;
      next = r.next_before;
      error = null;
      loaded = true;
    } catch (e) {
      if (g === gen) error = docError(e);
    }
  }

  function setFilter(patch: Partial<DocFilters>) {
    go(['docs'], filterQuery({ ...filters, ...patch }));
  }

  // --- search ----------------------------------------------------------------
  let result = $state.raw<DocSearchResult | null>(null);

  async function loadSearch() {
    const g = ++gen;
    try {
      const r = await get<DocSearchResult>(`/v1/docs/search${qs({ q, team: team || undefined, status: query.get('status') || undefined, kind: filters.kind || undefined, limit: 50 })}`);
      if (g !== gen) return;
      result = r;
      error = null;
      loaded = true;
    } catch (e) {
      if (g === gen) error = docError(e);
    }
  }

  // Load on open, when the URL changes and on every round of doc events.
  $effect(() => {
    void query.toString();
    void store.docSeq;
    loaded = false;
    if (mode === 'search') void loadSearch();
    else if (mode === 'list') void loadList();
    else void loadLanding();
  });

  const kindNames = $derived([...new Set([...kinds.map((k) => k.name), ...summary.kinds.keys()])]);
  const owners = $derived(store.seats.map((s) => s.name));
</script>

<div class="page docs">
  <div class="page-head">
    <h1>{t('docs.title')}</h1>
    <span class="spacer"></span>
    <form class="row search" onsubmit={search} role="search">
      <input type="search" data-search placeholder={t('docs.search')} aria-label={t('docs.search')} bind:value={searchDraft} data-testid="docs-search" />
      <label class="check"><input type="checkbox" bind:checked={searchAll} />{t('docs.searchAll')}</label>
      <button class="small" type="submit">{t('docs.searchGo')}</button>
    </form>
    <a class="btnlike" href={href(['docs', 'kinds'])}>{t('docs.kinds')}</a>
    <a class="btnlike" href={href(['docs', 'new'], { team: team || undefined })}>{t('docs.new')}</a>
  </div>

  {#if mode !== 'list'}
    <div class="tabs teams" role="tablist" aria-label={t('doc.field.team')}>
      <button role="tab" aria-selected={!team} onclick={() => go(['docs'], { q: q || undefined })}>{t('tasks.allTeams')}</button>
      {#each store.teams as tm (tm.name)}
        <button role="tab" aria-selected={team === tm.name} onclick={() => go(['docs'], { team: tm.name, q: q || undefined })}>{tm.name}</button>
      {/each}
    </div>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  {#if mode === 'search'}
    <p class="muted intro">{t('docs.searchIntro')}</p>
    {#if result}
      {#if result.titles_only}<p class="faint">{t('docs.titlesOnly')}</p>{/if}
      {#if !result.hits.length}
        <p class="empty">{t('docs.noHits', { q })}</p>
      {:else}
        <ul class="hits" data-testid="docs-hits">
          {#each result.hits as h, i (i)}
            <li class="hit card" data-doc={h.num}>
              <div class="row wrap">
                <a class="mono" href={docHref(h.num, h.anchor || undefined)}>{docId(h.num)}{#if h.anchor}#{h.anchor}{/if}</a>
                <a href={docHref(h.num, h.anchor || undefined)}><Vis text={h.title} /></a>
                <span class="chip neutral">{h.kind}</span>
                <span class="chip tone-{docStatusTone(h.status)}">{t(`doc.status.${h.status}`)}</span>
                {#if h.stale}<span class="chip attn" title={t('doc.stale.long')}>{t('doc.stale')}</span>{/if}
                {#if h.verified_at}<span class="faint" title={fullTime(h.verified_at, ui.lang)}>{t('docs.verifiedAt', { time: shortTime(h.verified_at, ui.now, ui.lang) })}</span>{/if}
              </div>
              {#if h.path}<div class="path muted"><Vis text={h.path} /></div>{/if}
              <div class="snippet"><Vis text={h.snippet} /></div>
            </li>
          {/each}
        </ul>
      {/if}
    {:else if !error}<p class="faint">{t('app.loading')}</p>{/if}
  {:else if mode === 'list'}
    <div class="row wrap filters" data-testid="docs-filters">
      <label>{t('doc.field.kind')}
        <select value={filters.kind} onchange={(e) => setFilter({ kind: (e.currentTarget as HTMLSelectElement).value })}>
          <option value="">{t('docs.anyKind')}</option>
          {#each kindNames as k}<option value={k}>{k}</option>{/each}
        </select>
      </label>
      <label>{t('doc.field.status')}
        <select value={filters.status} onchange={(e) => setFilter({ status: (e.currentTarget as HTMLSelectElement).value })}>
          <option value="">{t('docs.statusDefault')}</option>
          {#each DOC_STATUSES as s}<option value={s}>{t(`doc.status.${s}`)}</option>{/each}
          <option value="all">{t('docs.statusAll')}</option>
        </select>
      </label>
      <label>{t('doc.field.team')}
        <select value={filters.team} onchange={(e) => setFilter({ team: (e.currentTarget as HTMLSelectElement).value })}>
          <option value="">{t('tasks.allTeams')}</option>
          {#each store.teams as tm}<option value={tm.name}>{tm.name}</option>{/each}
        </select>
      </label>
      <label>{t('doc.field.owner')}
        <select value={filters.owner} onchange={(e) => setFilter({ owner: (e.currentTarget as HTMLSelectElement).value })}>
          <option value="">{t('docs.anyOwner')}</option>
          {#each owners as o}<option value={o}>{o}</option>{/each}
        </select>
      </label>
      <label>{t('doc.field.labels')}
        <input class="short" value={filters.label} onchange={(e) => setFilter({ label: (e.currentTarget as HTMLInputElement).value.trim() })} />
      </label>
      <label class="check"><input type="checkbox" checked={filters.stale} onchange={(e) => setFilter({ stale: (e.currentTarget as HTMLInputElement).checked })} />{t('doc.stale')}</label>
      <label class="check"><input type="checkbox" checked={filters.pinned} onchange={(e) => setFilter({ pinned: (e.currentTarget as HTMLInputElement).checked })} />{t('doc.pinned')}</label>
      <a class="sublink" href={href(['docs'], { team: filters.team || undefined })}>{t('docs.backToLanding')}</a>
    </div>
    {#if !loaded && !error}
      <p class="faint">{t('app.loading')}</p>
    {:else if !docs.length}
      <p class="empty">{t('docs.empty')}</p>
    {:else}
      <table class="list" data-testid="docs-list">
        <thead><tr><th>{t('doc.id')}</th><th>{t('doc.field.title')}</th><th>{t('doc.field.kind')}</th><th>{t('doc.field.status')}</th><th>{t('doc.field.owner')}</th><th>{t('doc.field.team')}</th><th>{t('task.updated')}</th></tr></thead>
        <tbody>
          {#each docs as d (d.num)}
            <tr data-doc={d.num}>
              <td class="mono"><a href={docHref(d.num)}>{docId(d.num)}</a></td>
              <td class="title">
                <a href={docHref(d.num)}><Vis text={d.title} /></a>
                {#if d.pinned}<span class="chip ack">{t('doc.pinned')}</span>{/if}
                {#if d.stale}<span class="chip attn" title={t('doc.stale.long')}>{t('doc.stale')}</span>{/if}
                {#if d.description}<div class="faint small"><Vis text={d.description} /></div>{/if}
              </td>
              <td>{d.kind}</td>
              <td><span class="chip tone-{docStatusTone(d.status)}">{t(`doc.status.${d.status}`)}</span></td>
              <td class="mono">{d.owner}</td>
              <td class="mono">{d.team ?? ''}</td>
              <td title={fullTime(d.updated_at, ui.lang)}>{shortTime(d.updated_at, ui.now, ui.lang)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
      {#if next}<button class="small more" onclick={() => loadList(true)}>{t('doc.more')}</button>{/if}
    {/if}
  {:else}
    <p class="muted intro">{t('docs.intro')}</p>
    {#if !loaded && !error}
      <p class="faint">{t('app.loading')}</p>
    {:else}
      <div class="landing" data-testid="docs-landing">
        <section class="card box">
          <h2>{t('docs.overview')}</h2>
          <p class="row wrap counts">
            <a href={href(['docs'], { team: team || undefined, status: 'draft' })} class="chip tone-warn" data-testid="docs-drafts">{t('docs.drafts', { n: summary.drafts })}</a>
            <a href={href(['docs'], { team: team || undefined, stale: '1' })} class="chip {summary.stale ? 'attn' : 'neutral'}" data-testid="docs-stale">{t('docs.staleCount', { n: summary.stale })}</a>
            <span class="faint">{t('docs.total', { n: teamMore ? `${summary.total}+` : summary.total })}</span>
          </p>
          <table class="kinds">
            <tbody>
              {#each kindNames as k}
                {@const info = kinds.find((x) => x.name === k)}
                <tr data-kind={k}>
                  <td><a href={href(['docs'], { kind: k, team: team || undefined })}>{k}</a></td>
                  <td class="num">{summary.kinds.get(k) ?? 0}</td>
                  <td class="faint">{info?.description ?? ''}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </section>
        <section class="card box">
          <h2>{t('docs.pinned')} <span class="count">{summary.pinned.length}</span></h2>
          {#if summary.pinned.length}
            <ul>{#each summary.pinned as d (d.num)}<li><DocChip doc={d} /></li>{/each}</ul>
          {:else}<p class="faint">{t('docs.noPinned')}</p>{/if}
        </section>
        <section class="card box wide">
          <h2>{t('docs.recent')}</h2>
          {#if teamDocs.length}
            <ul>
              {#each teamDocs.slice(0, 20) as d (d.num)}
                <li><DocChip doc={d} /> <span class="faint">{d.owner} · {shortTime(d.updated_at, ui.now, ui.lang)}</span></li>
              {/each}
            </ul>
            <a class="sublink" href={href(['docs'], { team: team || undefined, status: 'all' })}>{t('docs.all')}</a>
          {:else}<p class="faint">{t('docs.empty')}</p>{/if}
        </section>
      </div>
    {/if}
  {/if}
</div>

<style>
  .docs {
    max-width: 1200px;
  }
  .intro {
    margin: 6px 0 10px;
    font-size: 12px;
    max-width: 820px;
  }
  .search input[type='search'] {
    width: 240px;
    max-width: 100%;
  }
  .btnlike {
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    padding: 3px 10px;
    background: var(--surface);
    color: var(--text);
    white-space: nowrap;
  }
  .btnlike:hover {
    background: var(--surface-2);
    text-decoration: none;
  }
  .teams {
    margin: 4px 0 8px;
    flex-wrap: wrap;
  }
  .check {
    display: flex;
    gap: 4px;
    align-items: center;
    font-size: 12px;
    color: var(--muted);
  }
  .filters {
    margin: 6px 0 10px;
    align-items: flex-end;
  }
  .filters label:not(.check) {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  .short {
    width: 110px;
  }
  .sublink {
    font-size: 12px;
  }
  .list {
    width: 100%;
  }
  .list .title {
    overflow-wrap: anywhere;
  }
  .small {
    font-size: 12px;
  }
  .more {
    margin-top: 8px;
  }
  .landing {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 12px;
  }
  .box {
    padding: 10px 12px;
    min-width: 0;
  }
  .box.wide {
    grid-column: 1 / -1;
  }
  .box ul,
  .hits {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .counts {
    margin: 6px 0;
  }
  .counts a:hover {
    text-decoration: none;
  }
  .kinds {
    width: 100%;
  }
  .hit {
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .path {
    font-size: 12px;
  }
  .snippet {
    overflow-wrap: anywhere;
  }
  @media (max-width: 720px) {
    .docs {
      padding: 12px 16px;
    }
    .landing {
      grid-template-columns: minmax(0, 1fr);
    }
    .search {
      width: 100%;
      flex-wrap: wrap;
    }
    .search input[type='search'] {
      width: 100%;
    }
    .list {
      display: block;
      overflow-x: auto;
    }
  }
</style>
