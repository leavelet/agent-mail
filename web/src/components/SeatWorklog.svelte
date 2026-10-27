<script lang="ts">
  // A seat's worklog (`GET /v1/seats/{name}/worklog`): what the seat did,
  // newest first, filtered by task, kind and time, in pages, with the
  // counts per kind and the tasks touched over the same filters.
  import Vis from './Vis.svelte';
  import { get, qs, enc } from '../lib/api';
  import { href } from '../lib/router.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { fullTime, shortTime } from '../lib/time';
  import { WORKLOG_SINCE, docHref, docId } from '../lib/docs';
  import { docError } from '../lib/docops';
  import { parseTaskNum, taskId } from '../lib/tasks';
  import type { WorklogEntry, WorklogPage, WorklogSummary } from '../lib/types';

  let { seat }: { seat: string } = $props();

  let kind = $state('');
  let task = $state('');
  let since = $state('');
  let entries = $state.raw<WorklogEntry[]>([]);
  let next = $state<number | undefined>(undefined);
  let summary = $state.raw<WorklogSummary | null>(null);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let gen = 0;

  const path = $derived(`/v1/seats/${enc(seat)}/worklog`);
  const taskNum = $derived(task.trim() ? parseTaskNum(task) : null);
  const params = $derived({ kind: kind || undefined, task: taskNum ?? undefined, since: since || undefined });

  async function load(more = false) {
    const g = ++gen;
    try {
      const [page, sum] = await Promise.all([
        get<WorklogPage>(`${path}${qs({ ...params, before: more ? next : undefined, limit: 100 })}`),
        more ? Promise.resolve(summary) : get<WorklogSummary>(`${path}${qs({ ...params, summary: 1 })}`),
      ]);
      if (g !== gen) return;
      entries = more ? [...entries, ...page.entries] : page.entries;
      next = page.next_before;
      summary = sum;
      error = null;
      loaded = true;
    } catch (e) {
      if (g === gen) error = docError(e);
    }
  }

  $effect(() => {
    void seat;
    void params;
    void load();
  });
  $effect(() => {
    void seat;
    kind = '';
    task = '';
    since = '';
  });
</script>

<div class="worklog" data-testid="seat-worklog">
  <div class="row wrap filters">
    <label>{t('worklog.kind')}
      <select bind:value={kind} data-testid="worklog-kind">
        <option value="">{t('worklog.anyKind')}</option>
        {#each summary?.kinds ?? [] as k (k.kind)}<option value={k.kind}>{k.kind} ({k.count})</option>{/each}
        {#if kind && !summary?.kinds.some((k) => k.kind === kind)}<option value={kind}>{kind}</option>{/if}
      </select>
    </label>
    <label>{t('worklog.task')}<input class="mono short" placeholder="t12" bind:value={task} data-testid="worklog-task" /></label>
    <label>{t('worklog.since')}
      <select bind:value={since}>
        {#each WORKLOG_SINCE as s}<option value={s}>{s ? t('worklog.last', { d: s }) : t('worklog.anyTime')}</option>{/each}
      </select>
    </label>
    <span class="spacer"></span>
    <button class="small ghost" onclick={() => load()}>{t('handover.reload')}</button>
  </div>
  {#if task.trim() && !taskNum}<p class="error">{t('task.err.badNumber')}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  {#if summary}
    <div class="summary row wrap" data-testid="worklog-summary">
      <span>{t('worklog.total', { n: summary.total })}</span>
      {#each summary.kinds.slice(0, 12) as k (k.kind)}
        <button class="chip neutral" class:sel={kind === k.kind} onclick={() => (kind = kind === k.kind ? '' : k.kind)}>{k.kind} {k.count}</button>
      {/each}
      {#if summary.tasks.length}
        <span class="faint">{t('worklog.tasks')}</span>
        {#each summary.tasks.slice(0, 10) as x (x.task)}
          <button class="chip neutral mono" class:sel={taskNum === x.task} onclick={() => (task = taskNum === x.task ? '' : taskId(x.task))}>{taskId(x.task)} · {x.count}</button>
        {/each}
      {/if}
    </div>
  {/if}

  {#if !loaded && !error}
    <p class="faint">{t('app.loading')}</p>
  {:else if !entries.length}
    <p class="empty">{t('worklog.empty')}</p>
  {:else}
    <ol class="timeline">
      {#each entries as e (e.id)}
        <li data-kind={e.kind}>
          <span class="when faint" title={fullTime(e.at, ui.lang)}>{shortTime(e.at, ui.now, ui.lang)}</span>
          <span class="chip {e.source === 'node' ? 'neutral' : 'ack'}" title={t(`worklog.source.${e.source}`)}>{e.kind}</span>
          <span class="text"><Vis text={e.text} /></span>
          {#if e.task}<a class="mono" href={href(['tasks', taskId(e.task)])}>{taskId(e.task)}</a>{/if}
          {#if e.doc}<a class="mono" href={docHref(e.doc)}>{docId(e.doc)}</a>{/if}
          {#if e.session}<span class="faint mono sess" title={e.session}>{e.session.slice(0, 8)}</span>{/if}
        </li>
      {/each}
    </ol>
    {#if next}<button class="small" onclick={() => load(true)} data-testid="worklog-more">{t('worklog.older')}</button>{/if}
  {/if}
</div>

<style>
  .worklog {
    display: flex;
    flex-direction: column;
    gap: 8px;
    overflow: auto;
    min-height: 0;
    padding: 6px 0;
  }
  .filters label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  .short {
    width: 90px;
  }
  .summary {
    font-size: 12px;
    gap: 4px 6px;
  }
  .summary button.chip {
    cursor: pointer;
    border: 1px solid transparent;
  }
  .summary button.sel {
    border-color: var(--accent);
    color: var(--accent);
  }
  .timeline {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
  }
  .timeline li {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
    padding: 3px 0;
    border-bottom: 1px solid var(--border);
  }
  .when {
    min-width: 92px;
  }
  .text {
    flex: 1;
    min-width: 200px;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
</style>
