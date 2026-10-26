<script lang="ts">
  // One card of the board: number, title, assignee, priority (a control
  // that sets it), due time, what the task waits for, the progress of its
  // child tasks and a `rejected` result.
  import Vis from './Vis.svelte';
  import { href } from '../lib/router.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { shortTime } from '../lib/time';
  import { PRIORITIES, overdue, taskId, waitsList } from '../lib/tasks';
  import type { Task, TaskPriority } from '../lib/types';

  let {
    task,
    busy = false,
    onpriority,
  }: { task: Task; busy?: boolean; onpriority: (p: TaskPriority) => void } = $props();

  const late = $derived(overdue(task, ui.now));
</script>

<div class="tcard prio-{task.priority}" data-task={task.num} data-state={task.state}>
  <div class="l1">
    <a class="num mono" href={href(['tasks', taskId(task.num)])}>{taskId(task.num)}</a>
    <a class="title" href={href(['tasks', taskId(task.num)])}><Vis text={task.title} /></a>
  </div>
  <div class="l2">
    <select
      class="prio"
      aria-label={t('task.priority.set', { id: taskId(task.num) })}
      title={t('task.priority')}
      value={task.priority}
      disabled={busy}
      draggable="false"
      onchange={(e) => onpriority((e.currentTarget as HTMLSelectElement).value as TaskPriority)}
    >
      {#each PRIORITIES as p}<option value={p}>{t(`task.priority.${p}`)}</option>{/each}
    </select>
    {#if task.assignee}<a class="mono who" href={href(['org', task.assignee])}>{task.assignee}</a>{:else}<span class="faint who">{t('task.unassigned')}</span>{/if}
    {#if task.due}<span class="due" class:late title={t('task.due')}>{late ? '⚠ ' : ''}{shortTime(task.due, ui.now, ui.lang)}</span>{/if}
    {#if task.progress}<span class="chip neutral progress" data-testid="progress" title={t('task.progress', { done: task.progress.done, total: task.progress.total })}>{task.progress.done}/{task.progress.total}</span>{/if}
    {#if task.result === 'rejected'}<span class="chip tone-danger" data-testid="rejected">{t('task.result.rejected')}</span>{/if}
  </div>
  {#if task.state === 'open' && task.waits_on?.length}
    <div class="wait" data-testid="waits-on">{t('task.waitsOn', { list: waitsList(task, t('task.okNeeded')) })}</div>
  {:else if task.state === 'blocked'}
    <div class="wait blocked" data-testid="waits-for" title={task.blocked_reason ?? ''}>
      {#if task.waiting_on}{t('task.waitsFor', { seat: task.waiting_on })}{:else}{t('task.blockedNoSeat')}{/if}
      {#if task.blocked_reason}<span class="reason">· <Vis text={task.blocked_reason} /></span>{/if}
    </div>
  {/if}
  {#if task.steal_requested_by}
    <div class="steal" data-testid="steal">{t('task.steal.asks', { seat: task.steal_requested_by })} · {shortTime(task.steal_requested_at, ui.now, ui.lang)}</div>
  {/if}
  {#if task.labels?.length}
    <div class="labels">{#each task.labels as l}<span class="chip neutral"><Vis text={l} /></span>{/each}</div>
  {/if}
</div>

<style>
  .tcard {
    background: var(--surface);
    border: 1px solid var(--border);
    border-left: 3px solid var(--border-strong);
    border-radius: var(--radius);
    padding: 6px 8px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .tcard.prio-urgent {
    border-left-color: var(--danger);
  }
  .tcard.prio-high {
    border-left-color: var(--warn);
  }
  .tcard.prio-low {
    border-left-color: var(--surface-3);
  }
  .l1 {
    display: flex;
    gap: 6px;
    align-items: baseline;
    min-width: 0;
  }
  .num {
    color: var(--muted);
    flex: none;
  }
  .title {
    color: var(--text);
    overflow-wrap: anywhere;
    font-weight: 550;
  }
  .l2 {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-wrap: wrap;
    font-size: 12px;
  }
  .prio {
    min-height: 22px;
    padding: 0 4px;
    font-size: 12px;
  }
  .prio-urgent .prio {
    color: var(--danger);
    font-weight: 600;
  }
  .who {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 120px;
  }
  .due {
    color: var(--muted);
    white-space: nowrap;
  }
  .due.late {
    color: var(--danger);
    font-weight: 600;
  }
  .wait {
    font-size: 12px;
    color: var(--warn);
    overflow-wrap: anywhere;
  }
  .wait.blocked {
    color: var(--danger);
  }
  .reason {
    color: var(--muted);
  }
  .steal {
    font-size: 12px;
    color: var(--notice);
    font-weight: 600;
  }
  .progress {
    font-variant-numeric: tabular-nums;
  }
  .labels {
    display: flex;
    gap: 3px;
    flex-wrap: wrap;
  }
</style>
