<script lang="ts">
  // The table of tasks: the filters of `GET /v1/tasks`, newest first, with
  // older pages loaded on demand (`before`).
  import Vis from './Vis.svelte';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { shortTime } from '../lib/time';
  import { untrack } from 'svelte';
  import { PRIORITIES, TASK_STATES, filterParams, overdue, stateTone, taskId, type TaskFilters } from '../lib/tasks';
  import { listTasks, taskError } from '../lib/taskops';
  import type { Task } from '../lib/types';

  let { filters = $bindable() }: { filters: TaskFilters } = $props();

  const PAGE = 50;
  const MAX_PAGE = 500;
  let tasks = $state.raw<Task[]>([]);
  let next = $state<number | undefined>(undefined);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let gen = 0;

  /** The first page; `more`: the next older page; `limit`: as many as
   * are shown (a reload after a change). */
  async function load(more = false, limit = PAGE) {
    const g = ++gen;
    loading = true;
    error = null;
    try {
      const r = await listTasks({ ...filterParams(filters), limit, before: more ? next : undefined });
      if (g !== gen) return;
      tasks = more ? [...tasks, ...r.tasks] : r.tasks;
      next = r.next_before;
    } catch (e) {
      if (g === gen) error = taskError(e);
    } finally {
      if (g === gen) loading = false;
    }
  }

  // The filters changed: the first page again (text input waits a moment).
  let timer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    void JSON.stringify(filters);
    clearTimeout(timer);
    timer = setTimeout(() => void load(), 250);
    return () => clearTimeout(timer);
  });

  // A task changed (it may now match the filters, or no longer): load the
  // rows shown so far again (at most the server's page limit).
  let seenSeq = store.taskSeq;
  $effect(() => {
    const seq = store.taskSeq;
    if (seq === seenSeq) return;
    seenSeq = seq;
    void load(false, Math.min(MAX_PAGE, Math.max(PAGE, untrack(() => tasks.length))));
  });

  const seats = $derived(store.seats.map((s) => s.name));
</script>

<form class="filters" onsubmit={(e) => e.preventDefault()} data-testid="task-filters">
  <label>{t('task.filter.state')}
    <select bind:value={filters.state}>
      <option value="">{t('task.filter.unfinished')}</option>
      <option value="all">{t('task.filter.all')}</option>
      {#each TASK_STATES as s}<option value={s}>{t(`task.state.${s}`)}</option>{/each}
    </select>
  </label>
  <label>{t('task.assignee')}
    <select bind:value={filters.assignee}>
      <option value="">{t('task.filter.any')}</option>
      <option value="none">{t('task.unassigned')}</option>
      {#each seats as s}<option value={s}>{s}</option>{/each}
    </select>
  </label>
  <label>{t('task.reporter')}
    <select bind:value={filters.reporter}>
      <option value="">{t('task.filter.any')}</option>
      {#each seats as s}<option value={s}>{s}</option>{/each}
    </select>
  </label>
  <label>{t('task.team')}
    <select bind:value={filters.team}>
      <option value="">{t('task.filter.any')}</option>
      {#each store.teams as tm}<option value={tm.name}>{tm.name}</option>{/each}
    </select>
  </label>
  <label>{t('task.priority')}
    <select bind:value={filters.priority}>
      <option value="">{t('task.filter.any')}</option>
      {#each PRIORITIES as p}<option value={p}>{t(`task.priority.${p}`)}</option>{/each}
    </select>
  </label>
  <label>{t('task.label')}<input class="short" bind:value={filters.label} /></label>
  <label>{t('task.parent')}<input class="short mono" placeholder="t12" bind:value={filters.parent} /></label>
  <label>{t('task.filter.dueBefore')}<input type="datetime-local" bind:value={filters.due_before} /></label>
  <label class="check"><input type="checkbox" bind:checked={filters.stale} />{t('task.filter.stale')}</label>
</form>
{#if error}<p class="error" role="alert">{error}</p>{/if}
<div class="scroll card">
  <table data-testid="task-table">
    <thead>
      <tr>
        <th>#</th><th>{t('task.title')}</th><th>{t('task.state')}</th><th>{t('task.priority')}</th><th>{t('task.assignee')}</th><th>{t('task.team')}</th><th>{t('task.due')}</th><th>{t('task.updated')}</th>
      </tr>
    </thead>
    <tbody>
      {#if !loading && tasks.length === 0}<tr><td colspan="8" class="faint">{t('task.none')}</td></tr>{/if}
      {#each tasks as task (task.num)}
        <tr data-task={task.num}>
          <td class="mono"><a href={href(['tasks', taskId(task.num)])}>{taskId(task.num)}</a></td>
          <td class="title">
            <a href={href(['tasks', taskId(task.num)])}><Vis text={task.title} /></a>
            {#if task.state === 'open' && task.waits_on?.length}<div class="sub warn">{t('task.waitsOn', { list: task.waits_on.map(taskId).join(', ') })}</div>{/if}
            {#if task.state === 'blocked' && task.waiting_on}<div class="sub danger">{t('task.waitsFor', { seat: task.waiting_on })}</div>{/if}
          </td>
          <td><span class="chip tone-{stateTone(task.state)}">{t(`task.state.${task.state}`)}</span></td>
          <td class:urgent={task.priority === 'urgent'}>{t(`task.priority.${task.priority}`)}</td>
          <td class="mono">{task.assignee ?? '—'}</td>
          <td class="mono">{task.team ?? ''}</td>
          <td class:late={overdue(task, ui.now)}>{task.due ? shortTime(task.due, ui.now, ui.lang) : ''}</td>
          <td class="faint">{shortTime(task.updated_at, ui.now, ui.lang)}</td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>
<div class="row pager">
  {#if loading}<span class="faint">{t('app.loading')}</span>{/if}
  {#if next && !loading}<button class="small" onclick={() => load(true)}>{t('task.older')}</button>{/if}
</div>

<style>
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 12px;
    margin-bottom: 10px;
    align-items: flex-end;
  }
  .filters label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  .filters label.check {
    flex-direction: row;
    align-items: center;
    gap: 4px;
    min-height: 28px;
  }
  .short {
    width: 110px;
  }
  .scroll {
    overflow-x: auto;
    max-width: 100%;
  }
  .title {
    min-width: 200px;
  }
  .sub {
    font-size: 12px;
  }
  .warn {
    color: var(--warn);
  }
  .danger {
    color: var(--danger);
  }
  .urgent,
  .late {
    color: var(--danger);
    font-weight: 600;
  }
  .pager {
    margin-top: 8px;
  }
</style>
