<script lang="ts">
  // The org's tasks: a board (one column per shown state) or a table with
  // the list filters. The view, the team and the filters are remembered in
  // this browser.
  import TaskBoard from '../components/TaskBoard.svelte';
  import TaskTable from '../components/TaskTable.svelte';
  import { get, qs } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { load as loadPref, save as savePref } from '../lib/storage';
  import { parseFilters, taskId } from '../lib/tasks';
  import { taskError } from '../lib/taskops';
  import type { Task, TaskBoard as Board } from '../lib/types';

  type View = 'board' | 'table';
  let view = $state<View>(loadPref('amail.tasks.view') === 'table' ? 'table' : 'board');
  let team = $state(loadPref('amail.tasks.team') ?? '');
  const saved = parseFilters(loadPref('amail.tasks.filters'));
  let filters = $state(saved);
  let search = $state(saved.text);

  $effect(() => savePref('amail.tasks.view', view));
  $effect(() => savePref('amail.tasks.team', team || null));
  $effect(() => savePref('amail.tasks.filters', JSON.stringify(filters)));
  // The search box is the table's text filter.
  $effect(() => {
    filters.text = search;
  });

  let board = $state.raw<Board | null>(null);
  let error = $state<string | null>(null);
  let dragging = false;
  let pending = false;
  let gen = 0;

  async function loadBoard() {
    if (dragging) {
      pending = true;
      return;
    }
    const g = ++gen;
    try {
      const b = await get<Board>(`/v1/tasks/board${qs({ team: team || undefined })}`);
      if (g !== gen) return;
      board = b;
      error = null;
    } catch (e) {
      if (g === gen) error = taskError(e);
    }
  }

  // Load on open, when the team changes and on every round of task events.
  $effect(() => {
    void team;
    void store.taskSeq;
    if (view === 'board') void loadBoard();
  });

  /** Put a task as the server answered it in its column (it may have moved
   * to another one). */
  function apply(task: Task) {
    if (!board) return;
    const cols = board.columns.map((c) => {
      const had = c.tasks.some((x) => x.num === task.num);
      if (c.state === task.state) {
        return { ...c, tasks: had ? c.tasks.map((x) => (x.num === task.num ? task : x)) : [...c.tasks, task], count: had ? c.count : c.count + 1 };
      }
      return had ? { ...c, tasks: c.tasks.filter((x) => x.num !== task.num), count: c.count - 1 } : c;
    });
    board = { ...board, columns: cols };
  }

  function setDragging(on: boolean) {
    dragging = on;
    if (!on && pending) {
      pending = false;
      void loadBoard();
    }
  }

  const needle = $derived(search.trim().toLowerCase());
  const shown = $derived.by(() => {
    if (!board) return [];
    if (!needle) return board.columns;
    return board.columns.map((c) => ({ ...c, tasks: c.tasks.filter((x) => x.title.toLowerCase().includes(needle) || taskId(x.num) === needle) }));
  });
</script>

<div class="page tasks">
  <div class="page-head">
    <h1>{t('tasks.title')}</h1>
    <div class="tabs" role="tablist" aria-label={t('tasks.view')}>
      <button role="tab" aria-selected={view === 'board'} onclick={() => (view = 'board')}>{t('tasks.board')}</button>
      <button role="tab" aria-selected={view === 'table'} onclick={() => (view = 'table')}>{t('tasks.table')}</button>
    </div>
    <span class="spacer"></span>
    {#if view === 'board'}
      <label class="row team">{t('task.team')}
        <select bind:value={team} aria-label={t('task.team')}>
          <option value="">{t('tasks.allTeams')}</option>
          {#each store.teams as tm}<option value={tm.name}>{tm.name}</option>{/each}
        </select>
      </label>
    {/if}
    <input type="search" class="search" data-search placeholder={view === 'board' ? t('tasks.searchBoard') : t('tasks.search')} aria-label={t('tasks.search')} bind:value={search} />
    <a class="btnlike" href={href(['tasks', 'new'])}>{t('tasks.new')}</a>
  </div>
  <p class="muted intro">{t('tasks.intro')}</p>
  {#if view === 'board'}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if !board && !error}
      <p class="faint">{t('app.loading')}</p>
    {:else if board}
      {#if board.overdue.length || board.stale.length}
        <p class="row wrap flags">
          {#if board.overdue.length}<span class="chip attn">{t('tasks.overdue', { n: board.overdue.length })}</span>{#each board.overdue.slice(0, 10) as x}<a class="mono" href={href(['tasks', taskId(x.num)])}>{taskId(x.num)}</a>{/each}{/if}
          {#if board.stale.length}<span class="chip ack" title={t('tasks.stale.long')}>{t('tasks.stale', { n: board.stale.length })}</span>{#each board.stale.slice(0, 10) as x}<a class="mono" href={href(['tasks', taskId(x.num)])}>{taskId(x.num)}</a>{/each}{/if}
        </p>
      {/if}
      <TaskBoard columns={shown} filtered={!!needle} onapply={apply} onreload={loadBoard} ondragging={setDragging} />
      {#if board.load.length}
        <details class="load">
          <summary>{t('tasks.load')}</summary>
          <table>
            <thead><tr><th>{t('task.assignee')}</th><th>{t('task.state.doing')}</th><th>{t('task.state.ready')}</th><th>{t('task.state.blocked')}</th><th>{t('task.state.open')}</th><th>{t('task.state.review')}</th></tr></thead>
            <tbody>
              {#each board.load as l (l.seat)}
                <tr><td class="mono"><a href={href(['org', l.seat])}>{l.seat}</a></td><td class="num">{l.doing}</td><td class="num">{l.ready}</td><td class="num">{l.blocked}</td><td class="num">{l.waiting ?? 0}</td><td class="num">{l.review ?? 0}</td></tr>
              {/each}
            </tbody>
          </table>
        </details>
      {/if}
    {/if}
  {:else}
    <TaskTable bind:filters />
  {/if}
</div>

<style>
  .tasks {
    max-width: none;
    min-width: 0;
  }
  .intro {
    margin: 0 0 10px;
    font-size: 12px;
    max-width: 820px;
  }
  .search {
    width: 220px;
    max-width: 100%;
  }
  .team {
    font-size: 12px;
    color: var(--muted);
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
  .flags {
    margin: 0 0 8px;
    font-size: 12px;
  }
  .load {
    margin-top: 12px;
    max-width: 640px;
  }
  .load summary {
    cursor: pointer;
    color: var(--muted);
  }
  @media (max-width: 720px) {
    .tasks {
      padding: 12px 16px;
    }
    .search {
      width: 100%;
    }
  }
</style>
