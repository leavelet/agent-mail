<script lang="ts">
  // A task's history (`GET /v1/tasks/{num}/events`), oldest first, loaded
  // in pages. `version` changes when the task changed: the loaded events
  // are loaded again.
  import { untrack } from 'svelte';
  import { get, qs } from '../lib/api';
  import { href } from '../lib/router.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { shortTime, fullTime } from '../lib/time';
  import { taskError, taskPath } from '../lib/taskops';
  import type { TaskEvent, TaskEventsPage } from '../lib/types';
  import { isKey } from '../lib/i18n';

  let { num, version, thread }: { num: number; version: number; thread?: number } = $props();

  const PAGE = 100;
  const MAX = 1000;
  let events = $state.raw<TaskEvent[]>([]);
  let next = $state<number | undefined>(undefined);
  let compacted = $state(0);
  let error = $state<string | null>(null);
  let gen = 0;

  async function load(more: boolean, limit = PAGE) {
    const g = ++gen;
    try {
      const after = more ? (events[events.length - 1]?.seq ?? 0) : 0;
      const r = await get<TaskEventsPage>(`${taskPath(num, '/events')}${qs({ after_seq: after, limit })}`);
      if (g !== gen) return;
      events = more ? [...events, ...r.events] : r.events;
      next = r.next_after_seq;
      compacted = r.events_compacted;
      error = null;
    } catch (e) {
      if (g === gen) error = taskError(e);
    }
  }

  $effect(() => {
    void num;
    void version;
    void load(false, Math.min(MAX, Math.max(PAGE, untrack(() => events.length))));
  });

  function fieldName(f: string | undefined): string {
    if (!f) return '';
    const k = `task.field.${f}`;
    return isKey(k) ? t(k) : f;
  }

  function value(v: string | undefined): string {
    return v === undefined || v === '' ? '—' : v;
  }

  function describe(e: TaskEvent): string {
    switch (e.kind) {
      case 'created':
        return t('task.ev.created');
      case 'changed':
        if (e.field === 'body') return t('task.ev.body');
        return t('task.ev.changed', { field: fieldName(e.field), from: value(e.from), to: value(e.to) });
      case 'assigned':
        return t('task.ev.assigned', { from: value(e.from), to: value(e.to) });
      case 'linked':
        return t('task.ev.linked', { thread: value(e.to) });
      case 'unlinked':
        return t('task.ev.unlinked', { thread: value(e.to) });
      case 'dependency_added':
        return t('task.ev.depAdded', { task: value(e.to) });
      case 'dependency_removed':
        return t('task.ev.depRemoved', { task: value(e.to) });
      case 'commented':
        return t('task.ev.commented');
      case 'reminder_sent':
        return t('task.ev.reminder', { kind: value(e.field), seat: value(e.to) });
      case 'unblocked':
        return t('task.ev.unblocked');
      case 'steal_requested':
        return t('task.ev.stealRequested', { seat: value(e.to), from: value(e.from) });
      case 'steal_declined':
        return t('task.ev.stealDeclined', { seat: value(e.to), from: value(e.from) });
      case 'steal_expired':
        return t('task.ev.stealExpired', { seat: value(e.to) });
      default:
        return e.kind;
    }
  }
</script>

<section class="history" data-testid="task-history">
  <h2>{t('task.history')}</h2>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if compacted}<p class="faint">{t('task.compacted', { n: compacted })}</p>{/if}
  <ol>
    {#each events as e (e.seq)}
      <li data-kind={e.kind}>
        <span class="at faint" title={fullTime(e.at, ui.lang)}>{shortTime(e.at, ui.now, ui.lang)}</span>
        <span class="mono who">{e.actor ?? t('task.ev.server')}{#if e.written_by}<span class="faint"> · {t('mail.writtenBy', { user: e.written_by })}</span>{/if}</span>
        <span class="what">
          {describe(e)}
          {#if e.message_id && e.kind === 'commented' && thread}<a href={href(['thread', thread], { m: e.message_id })}>#{e.message_id}</a>{/if}
          {#if e.note}<span class="faint">· {e.note}</span>{/if}
        </span>
      </li>
    {/each}
  </ol>
  {#if next}<button class="small" onclick={() => load(true)}>{t('task.moreEvents')}</button>{/if}
</section>

<style>
  ol {
    list-style: none;
    margin: 6px 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  li {
    display: grid;
    grid-template-columns: 110px 140px 1fr;
    gap: 8px;
    font-size: 12px;
  }
  .who {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .what {
    overflow-wrap: anywhere;
  }
  @media (max-width: 720px) {
    li {
      grid-template-columns: 1fr;
      gap: 0;
      padding-bottom: 4px;
      border-bottom: 1px solid var(--border);
    }
  }
</style>
