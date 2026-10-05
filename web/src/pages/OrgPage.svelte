<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { go } from '../lib/router.svelte';
  import { buildTree, flatten, parents, aggregate, ancestors } from '../lib/tree';
  import { virtualWindow, scrollToRow } from '../lib/virtual';
  import { effectiveState, needsPerson } from '../lib/presence';
  import { seatsWaitingForUser } from '../lib/attention';
  import { typing, moveIndex } from '../lib/keys';
  import { SEAT_STATES, type SeatState } from '../lib/types';
  import TreeRow from '../components/TreeRow.svelte';
  import SeatDetail from '../components/SeatDetail.svelte';

  let { selected, stateFilter }: { selected?: string; stateFilter?: string } = $props();

  type Filter = SeatState | 'needs';
  const ROW_H = 30;

  let nameFilter = $state('');
  // svelte-ignore state_referenced_locally
  let states = $state.raw<Set<Filter>>(initialStates(stateFilter));
  let collapsed = $state.raw<Set<string>>(new Set());
  let scrollTop = $state(0);
  let viewport = $state(600);
  let scroller: HTMLDivElement | undefined = $state();

  function initialStates(s: string | undefined): Set<Filter> {
    const ok = [...SEAT_STATES, 'needs'] as string[];
    return s && ok.includes(s) ? new Set([s as Filter]) : new Set();
  }

  $effect(() => {
    states = initialStates(stateFilter);
  });

  const tree = $derived(buildTree(store.seats));
  const waiting = $derived(seatsWaitingForUser(store.attention));
  const now = $derived(ui.now);

  const match = $derived.by(() => {
    const q = nameFilter.trim().toLowerCase();
    if (!q && states.size === 0) return undefined;
    return (name: string) => {
      const seat = store.seatMap.get(name);
      if (!seat) return false;
      if (q && !name.includes(q) && !(seat.display_name ?? '').toLowerCase().includes(q) && !(seat.description ?? '').toLowerCase().includes(q)) return false;
      if (states.size === 0) return true;
      if (seat.kind === 'user') return false;
      if (states.has('needs') && needsPerson(seat, now)) return true;
      return states.has(effectiveState(seat, now));
    };
  });

  const rows = $derived(flatten(tree, collapsed, match));
  const win = $derived(virtualWindow(scrollTop, viewport, ROW_H, rows.length));
  const agentCount = $derived(store.seats.filter((s) => s.kind === 'agent').length);

  const below = $derived(
    aggregate(tree, (name): Record<string, number> => {
      const seat = store.seatMap.get(name);
      if (!seat || seat.kind === 'user') return {};
      return {
        [effectiveState(seat, now)]: 1,
        needs: needsPerson(seat, now) ? 1 : 0,
        waiting: waiting.has(name) ? 1 : 0,
      };
    }),
  );

  function toggle(name: string) {
    const next = new Set(collapsed);
    if (next.has(name)) next.delete(name);
    else next.add(name);
    collapsed = next;
  }

  function toggleState(s: Filter) {
    const next = new Set(states);
    if (next.has(s)) next.delete(s);
    else next.add(s);
    states = next;
  }

  function select(name: string, replace = false) {
    go(['org', name], undefined, replace);
  }

  // Keep the selected seat visible: expand its ancestors and scroll to it.
  $effect(() => {
    if (!selected) return;
    const anc = ancestors(tree, selected);
    if (anc.some((a) => collapsed.has(a))) {
      const next = new Set(collapsed);
      for (const a of anc) next.delete(a);
      collapsed = next;
    }
  });

  function reveal(index: number) {
    if (!scroller || index < 0) return;
    const top = scrollToRow(index, scroller.scrollTop, scroller.clientHeight, ROW_H);
    if (top !== scroller.scrollTop) scroller.scrollTop = top;
  }

  function onKey(e: KeyboardEvent) {
    if (typing(e) || ui.helpOpen) return;
    const i = rows.findIndex((r) => r.name === selected);
    const next = moveIndex(e.key, i, rows.length);
    if (next !== null) {
      e.preventDefault();
      select(rows[next].name, true);
      reveal(next);
      return;
    }
    const row = i >= 0 ? rows[i] : undefined;
    if (!row) return;
    if ((e.key === 'ArrowLeft' || e.key === 'h') && row.hasChildren && row.expanded) {
      toggle(row.name);
      e.preventDefault();
    } else if ((e.key === 'ArrowRight' || e.key === 'l') && row.hasChildren && !row.expanded) {
      toggle(row.name);
      e.preventDefault();
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="org" class:split={!!selected}>
  <section class="treepane" aria-label={t('org.title')}>
    <div class="toolbar">
      <h1>{t('org.title')}</h1>
      <input
        type="search"
        data-search
        placeholder={t('org.filter')}
        aria-label={t('org.filter')}
        bind:value={nameFilter}
      />
      <div class="filters" role="group" aria-label={t('org.col.state')}>
        <button class="small" class:on={states.size === 0} onclick={() => (states = new Set())}>{t('org.allStates')}</button>
        {#each SEAT_STATES as s}
          <button class="small st-{s}" class:on={states.has(s)} aria-pressed={states.has(s)} onclick={() => toggleState(s)}>
            <span class="dot"></span>{t(`state.${s}`)}
          </button>
        {/each}
        <button class="small needs" class:on={states.has('needs')} aria-pressed={states.has('needs')} onclick={() => toggleState('needs')}>⚠ {t('flag.attention')}</button>
      </div>
      <span class="spacer"></span>
      <span class="faint num">{t('org.showing', { shown: rows.filter((r) => store.seatMap.get(r.name)?.kind === 'agent').length, total: agentCount })}</span>
      <button class="small ghost" onclick={() => (collapsed = new Set())}>{t('org.expandAll')}</button>
      <button class="small ghost" onclick={() => (collapsed = new Set(parents(tree)))}>{t('org.collapseAll')}</button>
    </div>
    <div class="head" aria-hidden="true">
      <span>{t('org.col.seat')}</span><span class="hm">{t('org.col.harness')}</span><span>{t('org.col.state')}</span><span></span><span class="r">{t('org.col.mail')}</span>
    </div>
    <div
      class="scroller"
      role="tree"
      aria-label={t('org.title')}
      bind:this={scroller}
      bind:clientHeight={viewport}
      onscroll={(e) => (scrollTop = (e.currentTarget as HTMLDivElement).scrollTop)}
    >
      {#if store.seats.length === 0}
        <div class="empty">{t('org.empty')}</div>
      {:else if rows.length === 0}
        <div class="empty">{t('org.noMatch')}</div>
      {:else}
        <div style="height: {win.padTop}px" aria-hidden="true"></div>
        {#each rows.slice(win.start, win.end) as row (row.name)}
          {@const seat = store.seatMap.get(row.name)}
          {#if seat}
            <TreeRow
              {row}
              {seat}
              counts={store.counts.get(row.name)}
              waiting={waiting.has(row.name)}
              below={below.get(row.name)}
              size={tree.size.get(row.name) ?? 0}
              selected={row.name === selected}
              {now}
              ontoggle={toggle}
              onselect={(n) => select(n)}
            />
          {/if}
        {/each}
        <div style="height: {win.padBottom}px" aria-hidden="true"></div>
      {/if}
    </div>
  </section>
  {#if selected}
    <section class="detailpane">
      {#key selected}
        <SeatDetail name={selected} />
      {/key}
    </section>
  {/if}
</div>

<style>
  .org {
    display: grid;
    grid-template-columns: 1fr;
    height: 100%;
    min-height: 0;
  }
  .org.split {
    grid-template-columns: minmax(380px, 46%) 1fr;
  }
  .treepane {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    container: tree / inline-size;
    border-right: 1px solid var(--border);
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px 8px;
    flex-wrap: wrap;
  }
  .toolbar input {
    width: 180px;
  }
  .filters {
    display: flex;
    gap: 3px;
    flex-wrap: wrap;
  }
  .filters button {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--muted);
  }
  .filters button.on {
    background: var(--accent-weak);
    border-color: var(--accent);
    color: var(--accent);
    font-weight: 600;
  }
  .filters .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--c, var(--faint));
  }
  .st-busy {
    --c: var(--st-busy);
  }
  .st-idle {
    --c: var(--st-idle);
  }
  .st-awaiting {
    --c: var(--st-awaiting);
  }
  .st-dormant {
    --c: var(--st-dormant);
  }
  .st-offline {
    --c: var(--st-offline);
  }
  .filters .needs {
    color: var(--danger);
  }
  .head {
    display: grid;
    grid-template-columns: minmax(180px, 1fr) minmax(0, 170px) 128px minmax(0, 220px) 72px;
    gap: 8px;
    padding: 4px 10px 4px 26px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--faint);
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
  }
  .head .r {
    text-align: right;
  }
  @container tree (max-width: 720px) {
    .head {
      grid-template-columns: minmax(140px, 1fr) 118px minmax(0, 150px) 60px;
    }
    .head .hm {
      display: none;
    }
  }
  @container tree (max-width: 480px) {
    .head {
      display: none;
    }
  }
  .scroller {
    flex: 1;
    overflow-y: auto;
    min-height: 0;
    background: var(--surface);
  }
  .detailpane {
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
  @media (max-width: 1000px) {
    .org.split {
      grid-template-columns: 1fr;
    }
    .org.split .treepane {
      display: none;
    }
  }
</style>
